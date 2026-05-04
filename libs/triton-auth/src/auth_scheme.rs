// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared auth-scheme classifier for Triton HTTP services.
//!
//! Both the gateway and triton-api-server need to decide per-request
//! which authentication material a client presented. The gateway uses it
//! to branch between "verify JWT, resign with operator key" and
//! "passthrough verbatim so cloudapi verifies the HTTP Signature itself";
//! triton-api-server uses it on `/v1/*` handlers to route between the
//! Bearer (JWKS verify) and HTTP-Signature (mahi key lookup + draft-cavage
//! verify) paths. Keeping a single classifier here guarantees the two
//! services cannot drift out of agreement about what a request is.
//!
//! Precedence:
//! 1. `Authorization: Bearer <token>` → `Bearer(token)`.
//! 2. `Authorization: Signature <params>` → `HttpSignature(params)`.
//! 3. No (or unrecognized) Authorization header plus an `auth=<token>`
//!    cookie → `Bearer(token)` (browser UI session cookie).
//! 4. Nothing recognizable → `None`.
//!
//! The `Signature` header deliberately wins over a cookie so a client
//! that explicitly signed *this* request is not silently forced onto the
//! JWT path by a leftover browser session.

/// Classified authentication material extracted from a request.
///
/// The variants carry the underlying token or signature parameters so
/// handlers don't have to re-parse the `Authorization` header themselves.
/// Callers that only need the category can use `matches!()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthScheme {
    /// `Authorization: Bearer <token>` or `auth=<token>` cookie.
    ///
    /// Contains the raw token string with neither the `Bearer ` prefix nor
    /// the `auth=` cookie-key prefix.
    Bearer(String),
    /// `Authorization: Signature keyId="...",algorithm="...",headers="...",signature="..."`.
    ///
    /// Contains the full Authorization header value with the leading
    /// `Signature ` prefix stripped so callers can feed it straight into
    /// [`crate::http_sig::parse_signature_params`].
    HttpSignature(String),
    /// Nothing recognizable on the request.
    None,
}

/// Classify the auth material present on an incoming request.
///
/// See module docs for the precedence rules; this function is the single
/// point of truth for them across all Triton HTTP services.
pub fn classify(headers: &http::HeaderMap) -> AuthScheme {
    if let Some(auth) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return AuthScheme::Bearer(token.to_string());
        }
        if let Some(params) = auth.strip_prefix("Signature ") {
            // A bare `Signature` (no params) is pathological: every real
            // client sends keyId/algorithm/signature after the scheme name.
            // Treat it as unrecognized.
            if !params.is_empty() {
                return AuthScheme::HttpSignature(params.to_string());
            }
        }
    }
    // No (or unrecognized) Authorization header. Browser UI sessions put
    // the JWT in an `auth=` cookie rather than a header; honor that so a
    // signed-in browser can still call /v1/* endpoints.
    if let Some(token) = extract_auth_cookie(headers) {
        return AuthScheme::Bearer(token);
    }
    AuthScheme::None
}

/// Pull `auth=<token>` out of the `Cookie` header, regardless of its
/// position in the list.
fn extract_auth_cookie(headers: &http::HeaderMap) -> Option<String> {
    let cookie = headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())?;
    for part in cookie.split(';') {
        if let Some(value) = part.trim().strip_prefix("auth=") {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderMap, HeaderValue, header};

    fn headers_with(auth: Option<&str>, cookie: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(a) = auth {
            h.insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(a).expect("valid header value"),
            );
        }
        if let Some(c) = cookie {
            h.insert(
                header::COOKIE,
                HeaderValue::from_str(c).expect("valid cookie"),
            );
        }
        h
    }

    #[test]
    fn bearer_header_extracts_token_without_prefix() {
        let h = headers_with(Some("Bearer eyJ.jwt.token"), None);
        assert_eq!(
            classify(&h),
            AuthScheme::Bearer("eyJ.jwt.token".to_string())
        );
    }

    #[test]
    fn signature_header_extracts_params_without_prefix() {
        let value = r#"keyId="/user/keys/fp",algorithm="rsa-sha256",headers="date (request-target)",signature="abc=""#;
        let h = headers_with(Some(&format!("Signature {value}")), None);
        assert_eq!(classify(&h), AuthScheme::HttpSignature(value.to_string()));
    }

    #[test]
    fn auth_cookie_without_authorization_header_is_bearer() {
        // Browser UI sessions store the JWT in a cookie rather than a
        // header; a signed-in browser must still reach /v1/* endpoints.
        let h = headers_with(None, Some("auth=eyJ.jwt.token; other=1"));
        assert_eq!(
            classify(&h),
            AuthScheme::Bearer("eyJ.jwt.token".to_string())
        );
    }

    #[test]
    fn auth_cookie_not_first_in_list_still_detected() {
        // Regression guard: splitting logic must not require `auth=` to be
        // the leading cookie.
        let h = headers_with(None, Some("session=abc; auth=jwt; tracking=xyz"));
        assert_eq!(classify(&h), AuthScheme::Bearer("jwt".to_string()));
    }

    #[test]
    fn signature_header_wins_over_auth_cookie() {
        // A client presenting both an HTTP-Sig header and an auth cookie
        // is pathological, but the explicit scheme for THIS request must
        // take precedence over any background session.
        let value = r#"keyId="/user/keys/fp",algorithm="rsa-sha256""#;
        let h = headers_with(Some(&format!("Signature {value}")), Some("auth=jwt"));
        assert_eq!(classify(&h), AuthScheme::HttpSignature(value.to_string()));
    }

    #[test]
    fn bearer_header_wins_over_auth_cookie() {
        // Symmetric: explicit Bearer header beats any cookie.
        let h = headers_with(Some("Bearer x.y.z"), Some("auth=other"));
        assert_eq!(classify(&h), AuthScheme::Bearer("x.y.z".to_string()));
    }

    #[test]
    fn basic_auth_is_none() {
        let h = headers_with(Some("Basic dXNlcjpwYXNz"), None);
        assert_eq!(classify(&h), AuthScheme::None);
    }

    #[test]
    fn digest_auth_is_none() {
        let h = headers_with(
            Some(r#"Digest username="user",realm="r",nonce="n",uri="/",response="r""#),
            None,
        );
        assert_eq!(classify(&h), AuthScheme::None);
    }

    #[test]
    fn unknown_auth_scheme_is_none() {
        let h = headers_with(Some("Mystery token"), None);
        assert_eq!(classify(&h), AuthScheme::None);
    }

    #[test]
    fn bare_signature_token_is_none() {
        // `Signature` with no parameters is not a valid HTTP-Sig
        // authorization value; require the space+params that every real
        // signed request carries.
        let h = headers_with(Some("Signature"), None);
        assert_eq!(classify(&h), AuthScheme::None);
    }

    #[test]
    fn signature_with_trailing_space_but_no_params_is_none() {
        // Belt-and-braces: "Signature " with an empty param string after
        // the space is still not a real signed request.
        let h = headers_with(Some("Signature "), None);
        assert_eq!(classify(&h), AuthScheme::None);
    }

    #[test]
    fn no_auth_and_no_cookie_is_none() {
        let h = headers_with(None, None);
        assert_eq!(classify(&h), AuthScheme::None);
    }

    #[test]
    fn cookie_without_auth_key_is_none() {
        // Cookie header present but no `auth=` key inside.
        let h = headers_with(None, Some("session=abc; tracking=xyz"));
        assert_eq!(classify(&h), AuthScheme::None);
    }
}
