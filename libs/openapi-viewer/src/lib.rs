// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! OpenAPI documentation viewers for Dropshot-based services.
//!
//! This crate provides utilities to serve interactive API documentation alongside
//! your Dropshot API. It supports three popular OpenAPI viewers:
//!
//! - **Swagger UI** - The classic OpenAPI documentation interface
//! - **Redoc** - Clean, responsive three-panel documentation
//! - **RapiDoc** - Modern, customizable API documentation
//!
//! All viewers are loaded from CDN (jsdelivr.net) to minimize binary size.
//!
//! # Usage
//!
//! 1. Add endpoints to your API trait for the OpenAPI spec and viewer(s)
//! 2. Implement the endpoints using this crate's helpers:
//!
//! ```rust,ignore
//! use openapi_viewer::{swagger_ui_html, redoc_html, rapidoc_html, CSP_OPENAPI_VIEWER};
//!
//! async fn get_swagger_ui(
//!     _rqctx: RequestContext<Self::Context>,
//! ) -> Result<Response<Body>, HttpError> {
//!     let html = swagger_ui_html("/api-docs/openapi.json", "My API");
//!     Response::builder()
//!         .status(200)
//!         .header("Content-Type", "text/html; charset=utf-8")
//!         .header("Content-Security-Policy", CSP_OPENAPI_VIEWER)
//!         .body(html.into())
//!         .map_err(|e| HttpError::for_internal_error(e.to_string()))
//! }
//! ```

/// Content-Security-Policy header value for OpenAPI viewer pages.
///
/// This CSP allows all three viewers (Swagger UI, Redoc, RapiDoc) to function:
/// - Scripts from self, unsafe-inline, unsafe-eval (needed by some viewers), and cdn.jsdelivr.net
/// - Styles from self, unsafe-inline, and cdn.jsdelivr.net
/// - Images from self, data: URIs, blob: URIs, and cdn.jsdelivr.net
/// - Fonts from self, data: URIs, and cdn.jsdelivr.net
/// - Workers from blob: (needed by some viewers)
/// - Default to self for everything else
pub const CSP_OPENAPI_VIEWER: &str = "default-src 'self'; \
    script-src 'self' 'unsafe-inline' 'unsafe-eval' cdn.jsdelivr.net; \
    style-src 'self' 'unsafe-inline' cdn.jsdelivr.net; \
    img-src 'self' data: blob: cdn.jsdelivr.net; \
    font-src 'self' data: cdn.jsdelivr.net; \
    worker-src blob:";

/// Legacy alias for backward compatibility.
#[deprecated(since = "0.2.0", note = "Use CSP_OPENAPI_VIEWER instead")]
pub const CSP_SWAGGER_UI: &str = CSP_OPENAPI_VIEWER;

/// Swagger UI version to use from CDN.
const SWAGGER_UI_VERSION: &str = "5";

// =============================================================================
// Swagger UI
// =============================================================================

/// Generates an HTML page that renders Swagger UI for the given OpenAPI spec URL.
///
/// Swagger UI is the classic OpenAPI documentation interface with a two-panel
/// layout showing endpoints on the left and details on the right.
///
/// # Arguments
///
/// * `spec_url` - The URL path where the OpenAPI JSON spec is served (e.g., "/api-docs/openapi.json")
/// * `title` - The title to display in the browser tab
///
/// # Returns
///
/// An HTML string containing the Swagger UI page.
///
/// # Example
///
/// ```rust
/// use openapi_viewer::swagger_ui_html;
///
/// let html = swagger_ui_html("/api-docs/openapi.json", "My API");
/// assert!(html.contains("swagger-ui"));
/// assert!(html.contains("/api-docs/openapi.json"));
/// ```
pub fn swagger_ui_html(spec_url: &str, title: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{title} - Swagger UI</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{version}/swagger-ui.css">
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{version}/swagger-ui-bundle.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{version}/swagger-ui-standalone-preset.js"></script>
  <script>
    window.onload = () => {{
      SwaggerUIBundle({{
        url: "{spec_url}",
        dom_id: '#swagger-ui',
        presets: [
          SwaggerUIBundle.presets.apis,
          SwaggerUIStandalonePreset
        ],
        layout: "StandaloneLayout"
      }});
    }};
  </script>
</body>
</html>"#,
        title = title,
        spec_url = spec_url,
        version = SWAGGER_UI_VERSION
    )
}

// =============================================================================
// Redoc
// =============================================================================

/// Generates an HTML page that renders Redoc for the given OpenAPI spec URL.
///
/// Redoc provides a clean, responsive three-panel documentation layout with
/// a navigation menu, endpoint details, and request/response examples.
///
/// # Arguments
///
/// * `spec_url` - The URL path where the OpenAPI JSON spec is served (e.g., "/api-docs/openapi.json")
/// * `title` - The title to display in the browser tab
///
/// # Returns
///
/// An HTML string containing the Redoc page.
///
/// # Example
///
/// ```rust
/// use openapi_viewer::redoc_html;
///
/// let html = redoc_html("/api-docs/openapi.json", "My API");
/// assert!(html.contains("redoc"));
/// assert!(html.contains("/api-docs/openapi.json"));
/// ```
pub fn redoc_html(spec_url: &str, title: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{title} - Redoc</title>
  <link href="https://fonts.googleapis.com/css?family=Montserrat:300,400,700|Roboto:300,400,700" rel="stylesheet">
  <style>
    body {{ margin: 0; padding: 0; }}
  </style>
</head>
<body>
  <redoc spec-url="{spec_url}"></redoc>
  <script src="https://cdn.jsdelivr.net/npm/redoc@latest/bundles/redoc.standalone.js"></script>
</body>
</html>"#,
        title = title,
        spec_url = spec_url
    )
}

// =============================================================================
// RapiDoc
// =============================================================================

/// Generates an HTML page that renders RapiDoc for the given OpenAPI spec URL.
///
/// RapiDoc is a modern, customizable API documentation viewer built as a
/// web component. It offers various themes and layout options.
///
/// # Arguments
///
/// * `spec_url` - The URL path where the OpenAPI JSON spec is served (e.g., "/api-docs/openapi.json")
/// * `title` - The title to display in the browser tab
///
/// # Returns
///
/// An HTML string containing the RapiDoc page.
///
/// # Example
///
/// ```rust
/// use openapi_viewer::rapidoc_html;
///
/// let html = rapidoc_html("/api-docs/openapi.json", "My API");
/// assert!(html.contains("rapi-doc"));
/// assert!(html.contains("/api-docs/openapi.json"));
/// ```
pub fn rapidoc_html(spec_url: &str, title: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{title} - RapiDoc</title>
  <script type="module" src="https://cdn.jsdelivr.net/npm/rapidoc@latest/dist/rapidoc-min.js"></script>
</head>
<body>
  <rapi-doc
    spec-url="{spec_url}"
    theme="light"
    render-style="read"
    schema-style="table"
    show-header="false"
  ></rapi-doc>
</body>
</html>"#,
        title = title,
        spec_url = spec_url
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Swagger UI tests
    // =========================================================================

    #[test]
    fn test_swagger_ui_html_contains_spec_url() {
        let html = swagger_ui_html("/api-docs/openapi.json", "Test API");
        assert!(html.contains("/api-docs/openapi.json"));
    }

    #[test]
    fn test_swagger_ui_html_contains_title() {
        let html = swagger_ui_html("/spec.json", "My Cool API");
        assert!(html.contains("My Cool API - Swagger UI"));
    }

    #[test]
    fn test_swagger_ui_html_contains_cdn_links() {
        let html = swagger_ui_html("/spec.json", "Test");
        assert!(html.contains("cdn.jsdelivr.net/npm/swagger-ui-dist@"));
        assert!(html.contains("swagger-ui.css"));
        assert!(html.contains("swagger-ui-bundle.js"));
        assert!(html.contains("swagger-ui-standalone-preset.js"));
    }

    #[test]
    fn test_swagger_ui_html_contains_swagger_ui_div() {
        let html = swagger_ui_html("/spec.json", "Test");
        assert!(html.contains("<div id=\"swagger-ui\"></div>"));
    }

    // =========================================================================
    // Redoc tests
    // =========================================================================

    #[test]
    fn test_redoc_html_contains_spec_url() {
        let html = redoc_html("/api-docs/openapi.json", "Test API");
        assert!(html.contains("/api-docs/openapi.json"));
    }

    #[test]
    fn test_redoc_html_contains_title() {
        let html = redoc_html("/spec.json", "My Cool API");
        assert!(html.contains("My Cool API - Redoc"));
    }

    #[test]
    fn test_redoc_html_contains_cdn_links() {
        let html = redoc_html("/spec.json", "Test");
        assert!(html.contains("cdn.jsdelivr.net/npm/redoc@latest"));
        assert!(html.contains("redoc.standalone.js"));
    }

    #[test]
    fn test_redoc_html_contains_redoc_element() {
        let html = redoc_html("/spec.json", "Test");
        assert!(html.contains("<redoc spec-url="));
    }

    // =========================================================================
    // RapiDoc tests
    // =========================================================================

    #[test]
    fn test_rapidoc_html_contains_spec_url() {
        let html = rapidoc_html("/api-docs/openapi.json", "Test API");
        assert!(html.contains("/api-docs/openapi.json"));
    }

    #[test]
    fn test_rapidoc_html_contains_title() {
        let html = rapidoc_html("/spec.json", "My Cool API");
        assert!(html.contains("My Cool API - RapiDoc"));
    }

    #[test]
    fn test_rapidoc_html_contains_cdn_links() {
        let html = rapidoc_html("/spec.json", "Test");
        assert!(html.contains("cdn.jsdelivr.net/npm/rapidoc@latest"));
        assert!(html.contains("rapidoc-min.js"));
    }

    #[test]
    fn test_rapidoc_html_contains_rapidoc_element() {
        let html = rapidoc_html("/spec.json", "Test");
        assert!(html.contains("<rapi-doc"));
        assert!(html.contains("spec-url="));
    }

    // =========================================================================
    // CSP tests
    // =========================================================================

    #[test]
    fn test_csp_header_allows_jsdelivr() {
        assert!(CSP_OPENAPI_VIEWER.contains("cdn.jsdelivr.net"));
    }

    #[test]
    fn test_csp_header_allows_unsafe_inline() {
        assert!(CSP_OPENAPI_VIEWER.contains("'unsafe-inline'"));
    }

    #[test]
    fn test_csp_header_allows_blob_workers() {
        assert!(CSP_OPENAPI_VIEWER.contains("worker-src blob:"));
    }
}
