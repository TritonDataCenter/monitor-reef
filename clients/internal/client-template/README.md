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
3. Update `build.rs` to point to your service's OpenAPI spec:
   - Change `let spec_path = "../../../openapi-specs/generated/example-api.json";`
   - To `let spec_path = "../../../openapi-specs/generated/your-api.json";`
4. Ensure your API spec has been generated: `cargo run -p openapi-manager -- generate`
5. Run `cargo build` to generate the client
6. Use the generated client library in your applications

## How It Works

The `build.rs` script:
1. Reads the OpenAPI specification from `openapi-specs/generated/` (checked into git)
2. Uses Progenitor to generate a type-safe Rust client at build time
3. The generated client is written to the build output directory
4. Your `src/lib.rs` includes and exports the generated client code

## Generated Client Structure

This template creates a **library crate**, not an application. The generated client is meant to be used as a dependency in other services or applications.

## Benefits

- **Type safety**: Progenitor generates type-safe clients matching your API exactly
- **Fast compilation**: Specs generated from API traits without compiling service implementations
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
- This client is generated from that spec
- Other services use this client to talk to your service
