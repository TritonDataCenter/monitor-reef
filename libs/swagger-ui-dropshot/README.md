# swagger-ui-dropshot

A lightweight helper crate for adding Swagger UI documentation to Dropshot-based services.

## Overview

This crate provides utilities to serve an interactive [Swagger UI](https://swagger.io/tools/swagger-ui/)
alongside your Dropshot API. It uses CDN-hosted Swagger UI assets from jsdelivr.net, keeping your
binary size small while providing a polished API documentation experience.

## Usage

### 1. Add the dependency

In your service's `Cargo.toml`:

```toml
[dependencies]
swagger-ui-dropshot = { path = "../../libs/swagger-ui-dropshot" }
```

### 2. Add endpoints to your API trait

In your API trait definition (e.g., `apis/your-api/src/lib.rs`):

```rust
use dropshot::{HttpResponseOk, RequestContext, HttpError};
use http::Response;

#[dropshot::api_description]
pub trait YourApi {
    type Context: Send + Sync + 'static;

    // ... your existing endpoints ...

    /// Get OpenAPI specification
    #[endpoint {
        method = GET,
        path = "/api-docs/openapi.json",
        tags = ["documentation"],
    }]
    async fn get_openapi_spec(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError>;

    /// Swagger UI
    #[endpoint {
        method = GET,
        path = "/swagger-ui",
        tags = ["documentation"],
    }]
    async fn get_swagger_ui(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError>;
}
```

### 3. Implement the endpoints in your service

In your service implementation (e.g., `services/your-service/src/main.rs`):

```rust
use swagger_ui_dropshot::{swagger_ui_html, CSP_SWAGGER_UI};

impl YourApi for YourServiceImpl {
    type Context = ApiContext;

    // ... your existing endpoint implementations ...

    async fn get_openapi_spec(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<serde_json::Value>, HttpError> {
        // Embed the OpenAPI spec at compile time
        let spec: serde_json::Value = serde_json::from_str(include_str!(
            "../../../openapi-specs/generated/your-api.json"
        ))
        .map_err(|e| HttpError::for_internal_error(format!("Failed to parse OpenAPI spec: {}", e)))?;
        Ok(HttpResponseOk(spec))
    }

    async fn get_swagger_ui(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError> {
        let html = swagger_ui_html("/api-docs/openapi.json", "Your API");
        Response::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Content-Security-Policy", CSP_SWAGGER_UI)
            .body(html.into())
            .map_err(|e| HttpError::for_internal_error(format!("Failed to build response: {}", e)))
    }
}
```

### 4. Regenerate OpenAPI specs

After adding the endpoints, regenerate the OpenAPI specs:

```bash
make openapi-generate
```

### 5. Access the documentation

Once your service is running, visit:
- `/swagger-ui` - Interactive API documentation
- `/api-docs/openapi.json` - Raw OpenAPI specification

## API Reference

### `swagger_ui_html(spec_url: &str, title: &str) -> String`

Generates an HTML page that renders Swagger UI.

- `spec_url`: The URL path where the OpenAPI JSON spec is served (e.g., `/api-docs/openapi.json`)
- `title`: The title to display in the browser tab

### `CSP_SWAGGER_UI`

A Content-Security-Policy header value that allows Swagger UI to load assets from jsdelivr.net CDN.

## Security

The `CSP_SWAGGER_UI` constant provides a restrictive Content-Security-Policy that only allows:
- Scripts from `'self'`, `'unsafe-inline'` (for initialization), and `cdn.jsdelivr.net`
- Styles from `'self'`, `'unsafe-inline'`, and `cdn.jsdelivr.net`
- Images from `'self'`, `data:` URIs, and `cdn.jsdelivr.net`
- Fonts from `'self'` and `cdn.jsdelivr.net`

## License

This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
