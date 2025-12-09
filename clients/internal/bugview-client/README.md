<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Bugview Client

Auto-generated Rust client library for the Bugview API, built using Progenitor from the OpenAPI spec.

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
bugview-client = { path = "../../clients/internal/bugview-client" }
```

### Basic Example

```rust
use bugview_client::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new("https://smartos.org");

    // List public issues
    let response = client
        .get_issue_index_json()
        .send()
        .await?;

    for issue in response.issues {
        println!("{}: {}", issue.key, issue.summary);
    }

    // Get a specific issue
    let issue = client
        .get_issue_full_json()
        .key("OS-1234")
        .send()
        .await?;

    println!("Issue: {:?}", issue);

    Ok(())
}
```

### Pagination

```rust
let mut next_token: Option<String> = None;

loop {
    let mut request = client.get_issue_index_json();
    if let Some(token) = &next_token {
        request = request.next_page_token(token);
    }

    let response = request.send().await?;

    for issue in response.issues {
        println!("{}: {}", issue.key, issue.summary);
    }

    if response.is_last {
        break;
    }
    next_token = response.next_page_token;
}
```

## Available Methods

| Method | Description |
|--------|-------------|
| `get_issue_index_json()` | List public issues (paginated) |
| `get_issue_json()` | Get simplified issue details |
| `get_issue_full_json()` | Get complete issue details |
| `get_issue_index_html()` | Get HTML issue list |
| `get_label_index_html()` | Get HTML issue list filtered by label |
| `get_issue_html()` | Get HTML issue view |

## How It Works

This client is generated at build time:

1. `openapi-manager` generates `openapi-specs/generated/bugview-api.json` from the API trait
2. `build.rs` reads the spec and invokes Progenitor
3. Progenitor generates type-safe Rust client code
4. The generated code is included via `src/lib.rs`

## Related Crates

- `bugview-api` - API trait definition (source of the OpenAPI spec)
- `bugview-service` - Service implementation
- `bugview-cli` - CLI built on this client
