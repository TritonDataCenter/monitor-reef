// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use std::error::Error as StdError;

// `triton_gateway_client::Error` is the re-export of
// `progenitor_client::Error`; the gateway client crate is the only
// progenitor-generated dep triton-cli pulls in directly.
use triton_gateway_client::Error as ProgError;
use triton_gateway_client::types::Error as ApiError;

/// Error type for resource-not-found conditions.
///
/// Commands that fail because a named resource (instance, image, package, etc.)
/// cannot be resolved should return this error so that `main()` can exit with
/// code 3, matching the Node.js `triton` CLI convention.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ResourceNotFoundError(pub String);

/// Try to render a `progenitor_client::Error<gateway::Error>` somewhere in
/// `e`'s chain as a short, user-readable string. Returns `None` if no API
/// error is found, in which case the caller should fall back to
/// `format!("{e:#}")`.
///
/// Without this, progenitor's stock `Display` produces output like
/// `"Error Response: status: 405 ...; value: Error { code: Some(\"...\"),
/// error_code: None, message: Some(\"...\"), request_id: None }"` —
/// `{:?}` of the body struct leaks Rust internals into operator output.
/// We pull the typed body apart and reconstruct a node-triton-shaped
/// message instead.
pub fn render_api_error(e: &anyhow::Error) -> Option<String> {
    // anyhow's Error chain is iter<&dyn StdError + 'static>. The
    // Error<E> we care about is usually at the head (commands `?` the
    // result of `.send().await` directly, no `.context(...)`), but if
    // somebody wraps it later we still find it by walking.
    if let Some(api) = e.downcast_ref::<ProgError<ApiError>>() {
        return Some(format_progenitor(api));
    }
    for source in e.chain() {
        if let Some(api) = (source as &dyn StdError).downcast_ref::<ProgError<ApiError>>() {
            return Some(format_progenitor(api));
        }
    }
    None
}

fn format_progenitor(err: &ProgError<ApiError>) -> String {
    match err {
        ProgError::ErrorResponse(rv) => {
            let status = rv.status();
            let reason = status.canonical_reason().unwrap_or("");
            let body = rv.as_ref();
            let code = body
                .code
                .as_deref()
                .or(body.error_code.as_deref())
                .unwrap_or("");
            let message = body.message.as_deref().unwrap_or("");
            match (code.is_empty(), message.is_empty()) {
                (false, false) => format!("{} {reason}: {code}: {message}", status.as_u16()),
                (false, true) => format!("{} {reason}: {code}", status.as_u16()),
                (true, false) => format!("{} {reason}: {message}", status.as_u16()),
                (true, true) => format!("{} {reason}", status.as_u16()),
            }
        }
        // Server returned a body that didn't deserialize against the
        // generated `Error` schema. Show the raw bytes — they're more
        // useful than serde's "missing field" complaint, and the typed
        // path is now permissive enough that this should be rare.
        ProgError::InvalidResponsePayload(bytes, parse_err) => {
            let body = String::from_utf8_lossy(bytes);
            format!("invalid response payload ({parse_err}): {body}")
        }
        // Other variants (InvalidRequest, CommunicationError,
        // InvalidUpgrade, ResponseBodyError, UnexpectedResponse) have
        // adequate Display impls upstream — defer to those.
        other => other.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;
    use triton_gateway_client::ResponseValue;

    fn build_err_response(
        status: StatusCode,
        code: Option<&str>,
        error_code: Option<&str>,
        message: Option<&str>,
    ) -> ProgError<ApiError> {
        let body = ApiError {
            code: code.map(str::to_string),
            error_code: error_code.map(str::to_string),
            message: message.map(str::to_string),
            request_id: None,
        };
        ProgError::ErrorResponse(ResponseValue::new(body, status, HeaderMap::new()))
    }

    #[test]
    fn resource_not_found_downcast() {
        let err: anyhow::Error =
            ResourceNotFoundError("Instance not found: foo".to_string()).into();
        assert!(err.downcast_ref::<ResourceNotFoundError>().is_some());
        assert_eq!(err.to_string(), "Instance not found: foo");
    }

    #[test]
    fn other_errors_do_not_downcast_as_not_found() {
        let err = anyhow::anyhow!("connection refused");
        assert!(err.downcast_ref::<ResourceNotFoundError>().is_none());
    }

    #[test]
    fn render_cloudapi_shape_405() {
        // Reproduces `triton volume list` against a headnode where
        // VOLAPI isn't installed: cloudapi answers with 405 +
        // {"code": "MethodNotAllowedError", "message": "GET is not allowed"}.
        let err = build_err_response(
            StatusCode::METHOD_NOT_ALLOWED,
            Some("MethodNotAllowedError"),
            None,
            Some("GET is not allowed"),
        );
        let any: anyhow::Error = err.into();
        assert_eq!(
            render_api_error(&any).unwrap(),
            "405 Method Not Allowed: MethodNotAllowedError: GET is not allowed"
        );
    }

    #[test]
    fn render_dropshot_shape_uses_error_code() {
        // tritonapi (Dropshot-native) emits {error_code, message,
        // request_id}; the renderer should use error_code when code
        // is absent.
        let err = build_err_response(
            StatusCode::BAD_REQUEST,
            None,
            Some("InvalidArgument"),
            Some("foo is required"),
        );
        let any: anyhow::Error = err.into();
        assert_eq!(
            render_api_error(&any).unwrap(),
            "400 Bad Request: InvalidArgument: foo is required"
        );
    }

    #[test]
    fn render_returns_none_for_non_api_error() {
        let err = anyhow::anyhow!("connection refused");
        assert!(render_api_error(&err).is_none());
    }
}
