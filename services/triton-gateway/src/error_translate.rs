// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Gateway error-body translation.
//!
//! CloudAPI (Node.js Restify) and tritonapi (Rust Dropshot) use incompatible
//! error envelopes:
//!
//! - CloudAPI: `{ code (required), message?, request_id? }`
//! - tritonapi: `{ error_code?, message (required), request_id (required) }`
//!
//! When the gateway proxies a non-2xx CloudAPI response, we rewrite the body
//! into the tritonapi shape so every non-2xx response leaving the gateway
//! follows one canonical envelope — regardless of which upstream produced it.
//! This lets a future merged OpenAPI spec describe a single `Error` type
//! honestly (see the Phase 0 section of the gateway-client rollout plan).
//!
//! The HTTP status code is passed through verbatim; only the body is
//! translated. The `request_id` is preserved if the upstream supplied one,
//! otherwise the gateway's own per-request ID is injected.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// Minimum shape of a CloudAPI error body we can usefully decode. Extra fields
/// (errors[], etc.) are ignored — if we need them later we can extend.
#[derive(Debug, Deserialize)]
pub(crate) struct CloudapiError {
    pub code: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Tritonapi-shaped error body. Matches the Dropshot default error response
/// schema (see `openapi-specs/generated/triton-api.json` components/schemas/Error):
/// `message` and `request_id` are required, `error_code` is optional.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TritonapiError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub message: String,
    pub request_id: String,
}

/// Maximum bytes of raw upstream body to log when translation occurs.
/// Larger bodies are truncated in the log line; operators can correlate via
/// `request_id` and the upstream's own logs for the full payload.
const MAX_LOGGED_BODY_BYTES: usize = 2048;

/// Translate an upstream CloudAPI error body into a tritonapi-shaped `Error`.
///
/// `body` is the raw response-body bytes; `status` is the upstream HTTP status
/// (used to synthesize a message when the upstream body is missing or
/// non-JSON); `gateway_request_id` is injected when the upstream body did not
/// carry one of its own.
pub(crate) fn translate_cloudapi_error(
    body: &[u8],
    status: StatusCode,
    gateway_request_id: &str,
) -> TritonapiError {
    match serde_json::from_slice::<CloudapiError>(body) {
        Ok(c) => TritonapiError {
            error_code: Some(c.code),
            message: c
                .message
                .unwrap_or_else(|| status_reason(status).to_string()),
            request_id: c
                .request_id
                .unwrap_or_else(|| gateway_request_id.to_string()),
        },
        Err(_) => TritonapiError {
            error_code: Some("UpstreamError".to_string()),
            message: non_json_message(body, status),
            request_id: gateway_request_id.to_string(),
        },
    }
}

/// Build a uniform tritonapi-shaped error `Response`, for gateway-synthesized
/// error paths (missing upstream config, auth failures, etc.) that never hit
/// CloudAPI at all.
pub(crate) fn gateway_error_response(
    status: StatusCode,
    error_code: &str,
    message: impl Into<String>,
    request_id: &str,
) -> Response {
    let body = TritonapiError {
        error_code: Some(error_code.to_string()),
        message: message.into(),
        request_id: request_id.to_string(),
    };
    (status, Json(body)).into_response()
}

/// Produce a reasonable message when the upstream body is not valid
/// CloudAPI-shaped JSON. We include a (truncated) prefix of the raw body if
/// it's short and printable; otherwise we fall back to a status-only message.
fn non_json_message(body: &[u8], status: StatusCode) -> String {
    if body.is_empty() {
        return format!(
            "cloudapi returned empty body (status={} {})",
            status.as_u16(),
            status_reason(status)
        );
    }
    // Best-effort: if the body is short and valid UTF-8, include it so the
    // operator can see the raw text in the translated message. Otherwise
    // fall back to a status-only message; the full body is already in the
    // structured log line emitted at the call site.
    let text = match std::str::from_utf8(body) {
        Ok(s) => s.trim(),
        Err(_) => {
            return format!(
                "cloudapi returned non-JSON body (status={} {})",
                status.as_u16(),
                status_reason(status)
            );
        }
    };
    let truncated = if text.len() > 256 {
        format!("{}…", &text[..256])
    } else {
        text.to_string()
    };
    format!(
        "cloudapi returned non-JSON body (status={} {}): {}",
        status.as_u16(),
        status_reason(status),
        truncated
    )
}

/// `StatusCode::canonical_reason()` returns `Option<&'static str>`; this just
/// collapses the `None` case to a stable placeholder.
fn status_reason(status: StatusCode) -> &'static str {
    status.canonical_reason().unwrap_or("Error")
}

/// Truncate a byte slice for logging purposes. Returns the original slice when
/// already short enough, or a prefix when it isn't.
pub(crate) fn truncate_for_log(body: &[u8]) -> &[u8] {
    if body.len() <= MAX_LOGGED_BODY_BYTES {
        body
    } else {
        &body[..MAX_LOGGED_BODY_BYTES]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATEWAY_RID: &str = "gateway-rid-0000";

    #[test]
    fn translates_valid_cloudapi_error_with_all_fields() {
        let body = br#"{"code":"ResourceNotFound","message":"vm not found","request_id":"cloudapi-rid-abcd"}"#;
        let got = translate_cloudapi_error(body, StatusCode::NOT_FOUND, GATEWAY_RID);
        assert_eq!(
            got,
            TritonapiError {
                error_code: Some("ResourceNotFound".to_string()),
                message: "vm not found".to_string(),
                request_id: "cloudapi-rid-abcd".to_string(),
            }
        );
    }

    #[test]
    fn translates_valid_cloudapi_error_missing_optional_fields() {
        // `code` is the only required field in the CloudAPI shape.
        let body = br#"{"code":"InternalError"}"#;
        let got = translate_cloudapi_error(body, StatusCode::INTERNAL_SERVER_ERROR, GATEWAY_RID);
        assert_eq!(got.error_code.as_deref(), Some("InternalError"));
        // Missing message -> status reason.
        assert_eq!(got.message, "Internal Server Error");
        // Missing request_id -> gateway's ID.
        assert_eq!(got.request_id, GATEWAY_RID);
    }

    #[test]
    fn translates_non_json_body_to_upstream_error() {
        let body = b"<html>500 oh no</html>";
        let got = translate_cloudapi_error(body, StatusCode::BAD_GATEWAY, GATEWAY_RID);
        assert_eq!(got.error_code.as_deref(), Some("UpstreamError"));
        assert!(
            got.message.contains("non-JSON body"),
            "expected non-JSON marker in message, got: {}",
            got.message
        );
        assert!(
            got.message.contains("502"),
            "expected status code in message"
        );
        assert!(
            got.message.contains("<html>500 oh no</html>"),
            "expected raw body snippet in message, got: {}",
            got.message
        );
        assert_eq!(got.request_id, GATEWAY_RID);
    }

    #[test]
    fn translates_empty_body_to_upstream_error_with_status_reason() {
        let got = translate_cloudapi_error(b"", StatusCode::GATEWAY_TIMEOUT, GATEWAY_RID);
        assert_eq!(got.error_code.as_deref(), Some("UpstreamError"));
        assert!(got.message.contains("empty body"));
        assert!(got.message.contains("504"));
        assert_eq!(got.request_id, GATEWAY_RID);
    }

    #[test]
    fn truncates_long_raw_bodies_in_non_json_fallback() {
        let long_body = "x".repeat(5000);
        let got =
            translate_cloudapi_error(long_body.as_bytes(), StatusCode::BAD_GATEWAY, GATEWAY_RID);
        // 256-char cap + `…` + status prefix; overall must be well under the
        // raw 5000-byte input.
        assert!(
            got.message.len() < 500,
            "message too long: {}",
            got.message.len()
        );
        assert!(
            got.message.ends_with('…'),
            "expected ellipsis on truncation"
        );
    }

    #[test]
    fn truncate_for_log_caps_at_max_length() {
        let long = vec![b'A'; MAX_LOGGED_BODY_BYTES + 1000];
        assert_eq!(truncate_for_log(&long).len(), MAX_LOGGED_BODY_BYTES);
        let short = vec![b'B'; 10];
        assert_eq!(truncate_for_log(&short).len(), 10);
    }

    #[test]
    fn serialized_shape_matches_tritonapi_schema() {
        // `message` and `request_id` required; `error_code` optional and
        // omitted from wire when None.
        let with_code = TritonapiError {
            error_code: Some("ResourceNotFound".to_string()),
            message: "not found".to_string(),
            request_id: "rid-1".to_string(),
        };
        let v: serde_json::Value = serde_json::to_value(&with_code).unwrap();
        assert_eq!(v["error_code"], "ResourceNotFound");
        assert_eq!(v["message"], "not found");
        assert_eq!(v["request_id"], "rid-1");

        let without_code = TritonapiError {
            error_code: None,
            message: "boom".to_string(),
            request_id: "rid-2".to_string(),
        };
        let v: serde_json::Value = serde_json::to_value(&without_code).unwrap();
        assert!(
            v.get("error_code").is_none(),
            "error_code should be omitted when None"
        );
        assert_eq!(v["message"], "boom");
        assert_eq!(v["request_id"], "rid-2");
    }

    #[test]
    fn gateway_error_response_has_json_content_type_and_shape() {
        let resp = gateway_error_response(
            StatusCode::BAD_GATEWAY,
            "UpstreamUnavailable",
            "backend is down",
            "rid-xyz",
        );
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let ct = resp
            .headers()
            .get(http::header::CONTENT_TYPE)
            .expect("content-type present")
            .to_str()
            .unwrap();
        assert!(
            ct.starts_with("application/json"),
            "expected JSON content-type, got: {ct}"
        );
    }
}
