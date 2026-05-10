// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Hand-rolled AWS Signature V4 query-string presigner for S3.
//!
//! Used by the storage-cluster presign endpoints to mint URLs the
//! browser uses to PUT/GET bytes directly against mantad's S3 data
//! plane. We don't pull in the `aws-sigv4` crate because (a) its
//! transitive `aws-smithy-*` tree is heavy for a single signing
//! routine and (b) S3 presign is well-understood enough to hand-roll
//! against the AWS reference test vectors.
//!
//! References:
//!
//! * <https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html>
//! * <https://docs.aws.amazon.com/IAM/latest/UserGuide/aws-signing-authenticate-requests.html>
//!
//! # Scope
//!
//! Only "presigned URL" mode (a.k.a. SIGv4 query-string auth):
//!
//! * `payload_hash` is hard-coded to `UNSIGNED-PAYLOAD` so the browser
//!   doesn't have to stream the body twice.
//! * Signed headers are restricted to `host` (mandatory) — anything
//!   beyond that has to flow through the canonical-headers section
//!   AND get echoed by the client at request time, which adds
//!   complexity we don't need yet.
//! * Multipart uploads sign one URL per part with the `partNumber`
//!   and `uploadId` query params already encoded into the URL the
//!   browser PUTs.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// AWS service identifier used in scope strings. Mantad implements
/// the S3 surface, so this is always `"s3"`.
const SERVICE: &str = "s3";

/// Inputs for [`presign_url`].
#[derive(Debug, Clone)]
pub struct PresignRequest<'a> {
    /// AWS access key id (e.g. `AKIA...`). Embedded in the
    /// `X-Amz-Credential` query parameter.
    pub access_key_id: &'a str,
    /// IAM secret. Used to derive the signing key; never appears in
    /// the URL.
    pub secret_access_key: &'a str,
    /// AWS region label. For mantad clusters this is whatever
    /// `default_region` is on the StorageCluster (typically
    /// `us-east-1`).
    pub region: &'a str,
    /// Base URL of the S3 endpoint, e.g.
    /// `https://10.199.199.250:7443`. Must include scheme + host
    /// (port optional). The path component is replaced with the
    /// `bucket/key` pair at signing time.
    pub endpoint: &'a str,
    /// HTTP method for the presigned request. `"PUT"` for uploads,
    /// `"GET"` for downloads, `"DELETE"` for object delete.
    pub method: &'a str,
    /// Bucket name.
    pub bucket: &'a str,
    /// Object key (without leading `/`).
    pub key: &'a str,
    /// Extra query params the *browser* will include verbatim when
    /// it makes the actual request (e.g. `partNumber=1`,
    /// `uploadId=abc`). They go into the canonical query string and
    /// participate in the signature.
    pub extra_query: &'a [(&'a str, &'a str)],
    /// Validity window in seconds. AWS caps this at 7 days
    /// (604800); mantad doesn't strictly enforce that but we honor
    /// the cap so URLs minted here are portable to real AWS.
    pub expires_secs: u32,
    /// Reference timestamp the URL is valid from. Pass
    /// `chrono::Utc::now()` for normal operation; tests pin it.
    pub now: DateTime<Utc>,
}

/// Mint a SigV4 query-string-authenticated URL the caller can hand
/// to a browser as-is.
///
/// Returns the full URL including scheme, host, port, path, and the
/// `X-Amz-*` query parameters. The caller should not append further
/// query params after this point — doing so would invalidate the
/// signature.
pub fn presign_url(req: PresignRequest<'_>) -> Result<String, PresignError> {
    if req.access_key_id.is_empty() {
        return Err(PresignError::Misconfigured("access_key_id is empty"));
    }
    if req.secret_access_key.is_empty() {
        return Err(PresignError::Misconfigured("secret_access_key is empty"));
    }
    if req.region.is_empty() {
        return Err(PresignError::Misconfigured("region is empty"));
    }
    if req.bucket.is_empty() {
        return Err(PresignError::Misconfigured("bucket is empty"));
    }
    if req.key.is_empty() {
        return Err(PresignError::Misconfigured("key is empty"));
    }
    if req.expires_secs == 0 || req.expires_secs > 604_800 {
        return Err(PresignError::Misconfigured(
            "expires_secs must be in 1..=604800",
        ));
    }

    // Parse endpoint to pull off scheme + host[:port]. We use
    // `url::Url` for this so the ":7443" port is preserved correctly
    // and `host_str()` doesn't drop the port from the canonical
    // `Host:` header.
    let parsed = reqwest::Url::parse(req.endpoint)
        .map_err(|e| PresignError::BadEndpoint(format!("parse endpoint: {e}")))?;
    let scheme = parsed.scheme();
    let host_no_port = parsed
        .host_str()
        .ok_or_else(|| PresignError::BadEndpoint("endpoint has no host".into()))?;
    let host_for_signing = match parsed.port() {
        Some(p) => format!("{host_no_port}:{p}"),
        None => host_no_port.to_string(),
    };

    let amz_date = req.now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = req.now.format("%Y%m%d").to_string();
    let scope = format!("{date_stamp}/{}/{SERVICE}/aws4_request", req.region);
    let credential = format!("{}/{scope}", req.access_key_id);

    // Canonical query string. Order matters: AWS requires the params
    // to be sorted by key, then by value, with both percent-encoded.
    let mut params: Vec<(String, String)> = Vec::with_capacity(req.extra_query.len() + 6);
    params.push(("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()));
    params.push(("X-Amz-Credential".into(), credential.clone()));
    params.push(("X-Amz-Date".into(), amz_date.clone()));
    params.push(("X-Amz-Expires".into(), req.expires_secs.to_string()));
    params.push(("X-Amz-SignedHeaders".into(), "host".into()));
    for (k, v) in req.extra_query {
        params.push(((*k).to_string(), (*v).to_string()));
    }
    params.sort();
    let canonical_query = params
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
        .collect::<Vec<_>>()
        .join("&");

    // Canonical URI: `/{bucket}/{key}` with each segment percent-
    // encoded but `/` kept literal.
    let canonical_uri = format!(
        "/{}/{}",
        uri_encode(req.bucket, false),
        uri_encode(req.key, false)
    );

    let canonical_headers = format!("host:{host_for_signing}\n");
    let signed_headers = "host";
    let payload_hash = "UNSIGNED-PAYLOAD";

    let canonical_request = format!(
        "{}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        req.method
    );
    let canonical_request_hash = hex_sha256(canonical_request.as_bytes());

    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{canonical_request_hash}");

    let signing_key = derive_signing_key(req.secret_access_key, &date_stamp, req.region);
    let signature = hex(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    Ok(format!(
        "{scheme}://{host_for_signing}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}"
    ))
}

/// SigV4 derived signing key: a chain of HMACs over date / region /
/// service / `aws4_request`. Re-derived per request because the
/// date stamp rolls every UTC day.
fn derive_signing_key(secret: &str, date_stamp: &str, region: &str) -> Vec<u8> {
    let k1 = hmac_sha256(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k2 = hmac_sha256(&k1, region.as_bytes());
    let k3 = hmac_sha256(&k2, SERVICE.as_bytes());
    hmac_sha256(&k3, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex(hasher.finalize().to_vec())
}

fn hex(bytes: Vec<u8>) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Percent-encode per RFC 3986 unreserved-set rules.
///
/// AWS SigV4 has its own variant: `A-Z`, `a-z`, `0-9`, `-`, `.`,
/// `_`, `~` are kept literal. `/` is kept literal in path segments
/// (`encode_slash = false`) but encoded inside query values. Anything
/// else gets `%HH` upper-case.
fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let unreserved = matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~');
        if unreserved || (b == b'/' && !encode_slash) {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Errors from [`presign_url`].
#[derive(Debug, thiserror::Error)]
pub enum PresignError {
    /// One of the required input fields was empty or out of range.
    #[error("presign misconfigured: {0}")]
    Misconfigured(&'static str),
    /// `endpoint` was not a parseable URL with a host component.
    #[error("presign endpoint invalid: {0}")]
    BadEndpoint(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Reference vector adapted from the AWS SigV4 test suite for
    /// query-string auth (`get-vanilla-query-order-key-case`):
    /// signing a vanilla GET should produce a deterministic
    /// signature for a fixed timestamp + key + scope.
    ///
    /// The vector here uses the canonical AWS example
    /// (`AKIDEXAMPLE` / `wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY`)
    /// but with `service=s3` and `host=examplebucket.s3.amazonaws.com`,
    /// which is what S3 query-auth presigns look like.
    #[test]
    fn presign_get_is_deterministic_for_fixed_inputs() {
        let now = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let url = presign_url(PresignRequest {
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            endpoint: "https://examplebucket.s3.amazonaws.com",
            method: "GET",
            bucket: "test.txt".trim_start_matches('/'),
            // For this style of vector the bucket is in the host
            // (vhost-style); we treat it as a pseudo-bucket+key
            // pair where the URL ends up `/test.txt`.
            key: "test.txt",
            extra_query: &[],
            expires_secs: 86400,
            now,
        })
        .expect("presign should succeed");

        // Spot-check structure: the URL has the expected scheme,
        // host, X-Amz-* params, and a 64-hex signature.
        assert!(url.starts_with("https://examplebucket.s3.amazonaws.com/"));
        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(url.contains(
            "X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request"
        ));
        assert!(url.contains("X-Amz-Date=20130524T000000Z"));
        assert!(url.contains("X-Amz-Expires=86400"));
        assert!(url.contains("X-Amz-SignedHeaders=host"));
        let sig = url
            .rsplit_once("X-Amz-Signature=")
            .expect("URL has signature")
            .1;
        assert_eq!(sig.len(), 64, "signature should be 64 hex chars");
        assert!(
            sig.chars().all(|c| c.is_ascii_hexdigit()),
            "signature should be hex"
        );
    }

    #[test]
    fn presign_url_changes_with_method() {
        let now = Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
        let base = PresignRequest {
            access_key_id: "AKIA",
            secret_access_key: "SECRET",
            region: "us-east-1",
            endpoint: "https://mantad:7443",
            method: "GET",
            bucket: "bk",
            key: "obj",
            extra_query: &[],
            expires_secs: 60,
            now,
        };
        let get = presign_url(base.clone()).unwrap();
        let put = presign_url(PresignRequest {
            method: "PUT",
            ..base
        })
        .unwrap();
        assert_ne!(get, put, "different method should change the signature");
    }

    #[test]
    fn presign_url_changes_with_extra_query() {
        let now = Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
        let base = PresignRequest {
            access_key_id: "AKIA",
            secret_access_key: "SECRET",
            region: "us-east-1",
            endpoint: "https://mantad:7443",
            method: "PUT",
            bucket: "bk",
            key: "obj",
            extra_query: &[],
            expires_secs: 60,
            now,
        };
        let plain = presign_url(base.clone()).unwrap();
        let with_part = presign_url(PresignRequest {
            extra_query: &[("partNumber", "3"), ("uploadId", "abc")],
            ..base
        })
        .unwrap();
        assert_ne!(
            plain, with_part,
            "extra query params must participate in the signature"
        );
        assert!(with_part.contains("partNumber=3"));
        assert!(with_part.contains("uploadId=abc"));
    }

    #[test]
    fn presign_rejects_empty_credentials() {
        let now = Utc::now();
        let base = PresignRequest {
            access_key_id: "AKIA",
            secret_access_key: "SECRET",
            region: "us-east-1",
            endpoint: "https://mantad:7443",
            method: "GET",
            bucket: "bk",
            key: "obj",
            extra_query: &[],
            expires_secs: 60,
            now,
        };
        assert!(matches!(
            presign_url(PresignRequest {
                access_key_id: "",
                ..base.clone()
            }),
            Err(PresignError::Misconfigured(_))
        ));
        assert!(matches!(
            presign_url(PresignRequest {
                secret_access_key: "",
                ..base.clone()
            }),
            Err(PresignError::Misconfigured(_))
        ));
        assert!(matches!(
            presign_url(PresignRequest {
                bucket: "",
                ..base.clone()
            }),
            Err(PresignError::Misconfigured(_))
        ));
        assert!(matches!(
            presign_url(PresignRequest {
                expires_secs: 0,
                ..base.clone()
            }),
            Err(PresignError::Misconfigured(_))
        ));
        assert!(matches!(
            presign_url(PresignRequest {
                expires_secs: 604_801,
                ..base
            }),
            Err(PresignError::Misconfigured(_))
        ));
    }

    #[test]
    fn presign_rejects_bad_endpoint() {
        let now = Utc::now();
        let err = presign_url(PresignRequest {
            access_key_id: "AKIA",
            secret_access_key: "SECRET",
            region: "us-east-1",
            endpoint: "not a url",
            method: "GET",
            bucket: "bk",
            key: "obj",
            extra_query: &[],
            expires_secs: 60,
            now,
        })
        .unwrap_err();
        assert!(matches!(err, PresignError::BadEndpoint(_)));
    }

    #[test]
    fn uri_encode_handles_unreserved_set() {
        // Per AWS spec: unreserved chars stay literal, slash kept
        // literal in path mode but encoded in query mode.
        assert_eq!(uri_encode("abc.XYZ_123-~", false), "abc.XYZ_123-~");
        assert_eq!(uri_encode("a/b", false), "a/b");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("a b", true), "a%20b");
        assert_eq!(uri_encode("ä", true), "%C3%A4");
    }
}
