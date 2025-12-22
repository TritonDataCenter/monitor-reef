// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Swagger UI helper for Dropshot-based services.
//!
//! This crate provides utilities to serve Swagger UI alongside your Dropshot API.
//! It uses the CDN-hosted Swagger UI assets from jsdelivr.net.
//!
//! # Usage
//!
//! 1. Add two endpoints to your API trait:
//!    - `/api-docs/openapi.json` - serves your OpenAPI spec
//!    - `/swagger-ui` - serves the Swagger UI HTML page
//!
//! 2. Implement the endpoints using this crate's helpers:
//!
//! ```rust,ignore
//! use swagger_ui_dropshot::{swagger_ui_html, CSP_SWAGGER_UI};
//!
//! async fn get_swagger_ui(
//!     _rqctx: RequestContext<Self::Context>,
//! ) -> Result<Response<Body>, HttpError> {
//!     let html = swagger_ui_html("/api-docs/openapi.json", "My API");
//!     Response::builder()
//!         .status(200)
//!         .header("Content-Type", "text/html; charset=utf-8")
//!         .header("Content-Security-Policy", CSP_SWAGGER_UI)
//!         .body(html.into())
//!         .map_err(|e| HttpError::for_internal_error(e.to_string()))
//! }
//! ```

/// Content-Security-Policy header value for Swagger UI pages.
///
/// This CSP allows:
/// - Scripts from self, unsafe-inline (for Swagger UI initialization), and cdn.jsdelivr.net
/// - Styles from self, unsafe-inline (for Swagger UI styles), and cdn.jsdelivr.net
/// - Images from self, data: URIs (for embedded images), and cdn.jsdelivr.net
/// - Fonts from self and cdn.jsdelivr.net
/// - Default to self for everything else
pub const CSP_SWAGGER_UI: &str = "default-src 'self'; \
    script-src 'self' 'unsafe-inline' cdn.jsdelivr.net; \
    style-src 'self' 'unsafe-inline' cdn.jsdelivr.net; \
    img-src 'self' data: cdn.jsdelivr.net; \
    font-src 'self' cdn.jsdelivr.net";

/// Swagger UI version to use from CDN.
const SWAGGER_UI_VERSION: &str = "5";

/// Generates an HTML page that renders Swagger UI for the given OpenAPI spec URL.
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
/// use swagger_ui_dropshot::swagger_ui_html;
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
  <script>
    window.onload = () => {{
      SwaggerUIBundle({{
        url: "{spec_url}",
        dom_id: '#swagger-ui',
        presets: [
          SwaggerUIBundle.presets.apis,
          SwaggerUIBundle.SwaggerUIStandalonePreset
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn test_swagger_ui_html_contains_swagger_ui_div() {
        let html = swagger_ui_html("/spec.json", "Test");
        assert!(html.contains("<div id=\"swagger-ui\"></div>"));
    }

    #[test]
    fn test_csp_header_allows_jsdelivr() {
        assert!(CSP_SWAGGER_UI.contains("cdn.jsdelivr.net"));
    }
}
