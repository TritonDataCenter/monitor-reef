<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Client Template

This is a template for generating Rust client libraries from Dropshot API traits using Progenitor.

## Usage

1. Copy this directory to `clients/internal/your-service-client`
2. Update `Cargo.toml`:
   - Change `name = "client-template"` to `name = "your-service-client"`
   - Change `name = "client_template"` to `name = "your_service_client"` in the `[lib]` section
3. Register the client in `client-generator/src/main.rs`:
   - Add a `ClientConfig` entry to the `CLIENTS` array
   - Point `spec_path` to your API's OpenAPI spec
   - Point `output_path` to `clients/internal/your-service-client/src/generated.rs`
   - Configure generation settings (patches, derives, etc.)
4. Ensure your API spec has been generated: `make openapi-generate`
5. Generate the client code: `make clients-generate`
6. Use the generated client library in your applications

## How It Works

The `client-generator` tool:
1. Reads the OpenAPI specification from `openapi-specs/generated/` (checked into git)
2. Uses Progenitor to generate a type-safe Rust client
3. Formats the output with rustfmt and writes it to `src/generated.rs`
4. Your `src/lib.rs` includes and exports the generated client code via `mod generated`

The generated `src/generated.rs` is checked into git, making generated types visible to grep, IDE navigation, and code review.

## Generated Client Structure

This template creates a **library crate**, not an application. The generated client is meant to be used as a dependency in other services or applications.

## Benefits

- **Type safety**: Progenitor generates type-safe clients matching your API exactly
- **Visible code**: Generated code is checked in and visible in diffs, grep, and IDEs
- **Fast compilation**: No build.rs step — generated code compiles directly
- **Versioning**: dropshot-api-manager tracks API versions and validates compatibility
- **Consistent**: Same OpenAPI spec used for documentation, validation, and client generation
- **Clean dependencies**: Only includes what's needed for the generated client

## Example Usage

After building your client library, use it in another service:

```toml
# In your service's Cargo.toml
[dependencies]
your-service-client = { path = "../clients/internal/your-service-client" }
```

```rust
use your_service_client::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new("http://localhost:8080");

    // Use the generated client methods
    let response = client
        .your_endpoint()
        .send()
        .await?;

    println!("Response: {:?}", response);
    Ok(())
}
```

## Authentication

If your API requires authentication, use `new_with_client()` to provide a pre-configured `reqwest::Client`:

```rust
use reqwest::Client as ReqwestClient;
use your_service_client::Client;

let reqwest_client = ReqwestClient::builder()
    .default_headers({
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", api_token).parse()?,
        );
        headers
    })
    .build()?;

let client = Client::new_with_client("http://localhost:8080", reqwest_client);
```

## Relationship to Services

This template is designed for internal clients that talk to services in this monorepo:
- Service defines API trait in `apis/your-service-api`
- Service implements the trait in `services/your-service`
- `openapi-manager` generates OpenAPI spec from the trait
- `client-generator` produces `src/generated.rs` from that spec
- Other services use this client to talk to your service
