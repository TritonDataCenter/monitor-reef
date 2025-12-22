# openapi-viewer

A lightweight helper crate for adding interactive API documentation to Dropshot-based services.

## Overview

This crate provides utilities to serve interactive [OpenAPI](https://www.openapis.org/) documentation
alongside your Dropshot API. It supports three popular documentation viewers:

| Viewer | Description |
|--------|-------------|
| **Swagger UI** | The classic OpenAPI documentation interface with try-it-out functionality |
| **Redoc** | Clean, responsive three-panel documentation with a navigation menu |
| **RapiDoc** | Modern, customizable API documentation built as a web component |

All viewers are loaded from CDN (jsdelivr.net) to minimize binary size.

## Usage

### 1. Add the dependency

In your service's `Cargo.toml`:

```toml
[dependencies]
openapi-viewer = { path = "../../libs/openapi-viewer" }
```

### 2. Add endpoints to your API trait

In your API trait definition (e.g., `apis/your-api/src/lib.rs`):

```rust
use dropshot::{Body, HttpResponseOk, RequestContext, HttpError};
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

    /// Swagger UI documentation
    #[endpoint {
        method = GET,
        path = "/swagger-ui",
        tags = ["documentation"],
    }]
    async fn get_swagger_ui(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError>;

    // Optional: Add Redoc and/or RapiDoc endpoints
    #[endpoint {
        method = GET,
        path = "/redoc",
        tags = ["documentation"],
    }]
    async fn get_redoc(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError>;

    #[endpoint {
        method = GET,
        path = "/rapidoc",
        tags = ["documentation"],
    }]
    async fn get_rapidoc(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError>;
}
```

### 3. Implement the endpoints in your service

In your service implementation (e.g., `services/your-service/src/main.rs`):

```rust
use openapi_viewer::{swagger_ui_html, redoc_html, rapidoc_html, CSP_OPENAPI_VIEWER};

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
            .header("Content-Security-Policy", CSP_OPENAPI_VIEWER)
            .body(html.into())
            .map_err(|e| HttpError::for_internal_error(e.to_string()))
    }

    async fn get_redoc(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError> {
        let html = redoc_html("/api-docs/openapi.json", "Your API");
        Response::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Content-Security-Policy", CSP_OPENAPI_VIEWER)
            .body(html.into())
            .map_err(|e| HttpError::for_internal_error(e.to_string()))
    }

    async fn get_rapidoc(
        _rqctx: RequestContext<Self::Context>,
    ) -> Result<Response<Body>, HttpError> {
        let html = rapidoc_html("/api-docs/openapi.json", "Your API");
        Response::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Content-Security-Policy", CSP_OPENAPI_VIEWER)
            .body(html.into())
            .map_err(|e| HttpError::for_internal_error(e.to_string()))
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
- `/swagger-ui` - Swagger UI documentation
- `/redoc` - Redoc documentation
- `/rapidoc` - RapiDoc documentation
- `/api-docs/openapi.json` - Raw OpenAPI specification

## API Reference

### HTML Generators

#### `swagger_ui_html(spec_url: &str, title: &str) -> String`

Generates an HTML page that renders Swagger UI.

#### `redoc_html(spec_url: &str, title: &str) -> String`

Generates an HTML page that renders Redoc.

#### `rapidoc_html(spec_url: &str, title: &str) -> String`

Generates an HTML page that renders RapiDoc.

**Parameters for all functions:**
- `spec_url`: The URL path where the OpenAPI JSON spec is served (e.g., `/api-docs/openapi.json`)
- `title`: The title to display in the browser tab

### Constants

#### `CSP_OPENAPI_VIEWER`

A Content-Security-Policy header value that allows all three viewers to function properly.
Use this when serving any of the documentation pages.

## Security

The `CSP_OPENAPI_VIEWER` constant provides a Content-Security-Policy that allows:
- Scripts from `'self'`, `'unsafe-inline'`, `'unsafe-eval'`, and `cdn.jsdelivr.net`
- Styles from `'self'`, `'unsafe-inline'`, and `cdn.jsdelivr.net`
- Images from `'self'`, `data:` URIs, `blob:` URIs, and `cdn.jsdelivr.net`
- Fonts from `'self'`, `data:` URIs, and `cdn.jsdelivr.net`
- Workers from `blob:` (needed by some viewers)

## License

This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
