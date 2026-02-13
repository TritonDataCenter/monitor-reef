<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Testing and Validation Guide

## Required Test Types

All services must include:
- **Unit tests** for business logic
- **Integration tests** against actual HTTP endpoints
- **OpenAPI spec validation** (automated via openapi-manager)
- **Client compatibility tests** using generated clients
- **Real data fixtures** - Sample JSON responses from actual endpoints

## Test Data Management

Store sample JSON responses as test artifacts to ensure tests validate against real-world data:

```
tests/
├── fixtures/
│   ├── endpoint-name-success.json      # Successful response
│   ├── endpoint-name-error-404.json    # Error response
│   └── endpoint-name-list.json         # Collection response
└── integration/
    └── endpoint_tests.rs
```

**Guidelines:**
- Capture real responses from endpoints during development
- Include both success and error response examples
- Update fixtures when API response schemas change
- Use fixtures in both unit and integration tests
- Document the source/date of captured responses in comments

**Example:**
```rust
#[tokio::test]
async fn test_get_resource() {
    let fixture = include_str!("../fixtures/get-resource-success.json");
    let expected: Resource = serde_json::from_str(fixture).unwrap();
    // Test against this real data structure
}
```

## CI Check for Stale Specs

```bash
# Verify OpenAPI specs are up-to-date with trait definitions
make openapi-check

# This will fail if:
# - API traits changed but specs weren't regenerated
# - Specs in git don't match what would be generated
```

## Doctests Policy

- API trait crates (`apis/*`) and Progenitor-generated client crates include documentation examples that rustdoc treats as doctests.
- These examples are illustrative and are ignored by default in `cargo test` and CI.
- Forcing doctests to run (e.g., `cargo test -p bugview-client --doc -- --ignored`) will typically fail without a running HTTP service and async context; we intentionally do not run these in CI.
- Prefer adding runnable unit/integration tests in service crates for behavior verification.
