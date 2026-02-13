<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Consuming External APIs (Interim Migration Pattern)

When building new services that need to consume external/legacy APIs during migration.

## Philosophy

Use hand-written minimal clients instead of large auto-generated ones.

**Why?**
- Large OpenAPI specs (>1MB) often fail with Progenitor or have broken generated code
- Auto-generated clients from tools like `openapi` crate may have compilation errors
- You typically only need a small subset of endpoints
- Hand-written clients are easier to understand, debug, and maintain
- Faster compile times and smaller dependencies

## Example

See `services/bugview-service` which consumes the JIRA API.

```rust
// services/my-service/src/external_client.rs
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct ExternalApiClient {
    client: Client,
    base_url: String,
}

impl ExternalApiClient {
    pub fn new(base_url: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { client, base_url })
    }

    pub async fn get_resource(&self, id: &str) -> Result<Resource> {
        let url = format!("{}/api/resource/{}", self.base_url, id);
        let response = self.client.get(&url).send().await?;
        response.json().await
    }
}
```

## Steps

1. Store external OpenAPI spec in `openapi-specs/external/` (tracked in git)
2. Hand-write a minimal client with only the endpoints you need
3. Define your own API trait for the service you're building
4. Implement the trait using the external client
5. Generate OpenAPI spec for YOUR API (not the external one)

## Lessons Learned

- Don't try to use Progenitor on massive third-party specs
- The `jira_v3_openapi` crate (1.4.1) has broken imports and doesn't compile
- Hand-writing 3-5 endpoint wrappers takes less time than debugging generated code
- This pattern works great for migration: your NEW Rust service has a clean API while consuming the OLD API internally
