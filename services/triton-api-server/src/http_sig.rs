// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Pure (stateless, I/O-free) draft-cavage HTTP Signature verifier.
//!
//! This module implements the subset of draft-cavage-http-signatures that
//! CloudAPI and node-triton have standardised on:
//!
//! ```text
//! Authorization: Signature keyId="/:account/keys/:fp",algorithm="rsa-sha256",headers="date (request-target)",signature="<b64>"
//! ```
//!
//! The three layers below are intentionally I/O-free so they can be unit
//! tested without a server, a mahi fixture, or a clock. The
//! `/v1/auth/login-ssh` handler (a later slice) is the integration point:
//! it calls [`parse_signature_params`] on the Authorization header value,
//! resolves the `keyId` via mahi to a public OpenSSH blob, then uses
//! [`build_signing_string`] and [`verify_signature`] to decide 200/401.
//!
//! Deliberately out of scope here:
//! - Clock-skew enforcement on the `Date` header -- policy belongs to the
//!   endpoint handler, not the signature verifier (a long-running replay
//!   protection job wants different skew windows than an interactive login).
//! - `keyId` parsing -- the shape varies (account key vs sub-user key) and
//!   mahi lookup is an I/O concern.
//! - Network fetch of the public key -- same reason.

use base64::Engine;
// The `signature` crate's `Verifier` trait is re-exported by rsa, p256,
// p384 and ed25519-dalek -- one import is enough to bring the `verify`
// method into scope for every key type below.
use ed25519_dalek::Verifier as _;
use ssh_key::public::KeyData;

/// Errors surfaced by the HTTP-Signature pipeline.
///
/// Typed so the endpoint handler can map to the right HTTP status:
/// malformed input and missing required material are client errors (400),
/// a verifier reject is authentication failure (401), and a public key
/// blob that mahi gave us but we can't parse is an internal error (500).
#[derive(Debug, thiserror::Error)]
pub enum SigError {
    /// The `Authorization` value couldn't be parsed into keyId/algorithm/
    /// headers/signature. Malformed client input -- 400.
    #[error("malformed signature params: {0}")]
    Malformed(String),
    /// The signing-string builder was told to cover a header the request
    /// didn't actually carry. 400.
    #[error("request is missing signed header {0:?}")]
    MissingHeader(String),
    /// The `algorithm` parameter isn't one we're willing to verify. 400.
    #[error("unsupported signature algorithm {0:?}")]
    UnsupportedAlgorithm(String),
    /// The declared algorithm can't be used with this key (e.g.
    /// `rsa-sha256` with an Ed25519 key). 400.
    #[error("algorithm {algorithm:?} is not compatible with key type {key_type:?}")]
    AlgorithmKeyMismatch { algorithm: String, key_type: String },
    /// The cryptographic verification failed. 401.
    #[error("signature verification failed")]
    VerificationFailed,
    /// We couldn't turn the OpenSSH-format public-key blob into a typed
    /// key -- shouldn't happen for blobs mahi stores. 500.
    #[error("failed to parse public key: {0}")]
    KeyParseError(String),
}

// ---------------------------------------------------------------------------
// 1. Parser
// ---------------------------------------------------------------------------

/// The four fields we need off a draft-cavage `Authorization: Signature`
/// value after parsing.
///
/// `key_id` is passed through verbatim so the caller can decide whether
/// it's a `/{account}/keys/{fp}` or `/{account}/users/{user}/keys/{fp}`
/// shape. `headers` is lower-cased to canonical form at parse time so
/// the signing-string builder can do straight string comparisons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSignature {
    pub key_id: String,
    pub algorithm: String,
    pub headers: Vec<String>,
    pub signature: Vec<u8>,
}

/// Parse the value of an `Authorization: Signature <value>` header,
/// with the leading `Signature ` already stripped (the classifier does
/// that step).
///
/// Accepts the draft-cavage parameter list: comma-separated `key="value"`
/// pairs. Values may also appear unquoted, though the quoted form is what
/// every real signer we care about emits. Whitespace around commas and
/// equals signs is tolerated per the draft.
///
/// Unknown parameter names are ignored for forward compatibility. Repeated
/// known parameters are rejected -- the draft is ambiguous about which
/// should win, so erring on the side of refusing a suspicious signature
/// is the safer call.
pub fn parse_signature_params(authorization_value: &str) -> Result<ParsedSignature, SigError> {
    let mut key_id: Option<String> = None;
    let mut algorithm: Option<String> = None;
    let mut headers: Option<String> = None;
    let mut signature_b64: Option<String> = None;

    for (name, value) in parse_params(authorization_value)? {
        match name.as_str() {
            "keyid" => replace_once(&mut key_id, value, "keyId")?,
            "algorithm" => replace_once(&mut algorithm, value, "algorithm")?,
            "headers" => replace_once(&mut headers, value, "headers")?,
            "signature" => replace_once(&mut signature_b64, value, "signature")?,
            // Unknown params (e.g. `created=`, `expires=`) are tolerated;
            // a future draft-cavage revision may add parameters we don't
            // recognise yet.
            _ => {}
        }
    }

    let key_id = key_id.ok_or_else(|| SigError::Malformed("missing keyId".to_string()))?;
    // `algorithm` is technically optional in the draft ("use keyId's own
    // algorithm"), but our verifier needs to know the hash to use and we
    // don't want an attacker to be able to switch it out from under us
    // silently. Require it.
    let algorithm =
        algorithm.ok_or_else(|| SigError::Malformed("missing algorithm".to_string()))?;
    let signature_b64 =
        signature_b64.ok_or_else(|| SigError::Malformed("missing signature".to_string()))?;

    let headers = match headers {
        // Default per draft-cavage: if no `headers=` provided, the
        // signature covers only the `Date` header.
        None => vec!["date".to_string()],
        Some(hdr_string) => hdr_string
            .split_ascii_whitespace()
            .map(|s| s.to_ascii_lowercase())
            .collect(),
    };

    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.as_bytes())
        .map_err(|e| SigError::Malformed(format!("signature base64: {e}")))?;

    Ok(ParsedSignature {
        key_id,
        algorithm,
        headers,
        signature,
    })
}

/// Reject repeated known parameters so we never silently prefer one over
/// the other.
fn replace_once(slot: &mut Option<String>, value: String, name: &str) -> Result<(), SigError> {
    if slot.is_some() {
        return Err(SigError::Malformed(format!("duplicate {name} parameter")));
    }
    *slot = Some(value);
    Ok(())
}

/// Split a draft-cavage parameter string into `(name_lowercased, value)`
/// pairs. Handles quoted and unquoted values, whitespace around separators,
/// and basic `\"` escapes inside quoted strings.
fn parse_params(input: &str) -> Result<Vec<(String, String)>, SigError> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Skip leading whitespace / stray commas between pairs.
        while i < chars.len() && (chars[i].is_ascii_whitespace() || chars[i] == ',') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        // Read key (up to `=` or whitespace).
        let key_start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_ascii_whitespace() {
            i += 1;
        }
        if key_start == i {
            return Err(SigError::Malformed("expected parameter name".to_string()));
        }
        let key: String = chars[key_start..i]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        // Allow whitespace before `=`.
        while i < chars.len() && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            return Err(SigError::Malformed(format!(
                "expected '=' after parameter {key:?}"
            )));
        }
        i += 1; // consume '='
        // Allow whitespace after `=`.
        while i < chars.len() && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        // Read value -- quoted or bare.
        let value = if i < chars.len() && chars[i] == '"' {
            i += 1;
            let mut v = String::new();
            loop {
                if i >= chars.len() {
                    return Err(SigError::Malformed(format!(
                        "unterminated quoted value for {key:?}"
                    )));
                }
                match chars[i] {
                    '\\' => {
                        // Backslash-escape inside a quoted string: preserve
                        // the next character literally. Most signers use
                        // this only for `\"`, but we're lenient.
                        i += 1;
                        if i >= chars.len() {
                            return Err(SigError::Malformed(format!(
                                "dangling escape in value for {key:?}"
                            )));
                        }
                        v.push(chars[i]);
                        i += 1;
                    }
                    '"' => {
                        i += 1;
                        break;
                    }
                    c => {
                        v.push(c);
                        i += 1;
                    }
                }
            }
            v
        } else {
            let v_start = i;
            while i < chars.len() && chars[i] != ',' && !chars[i].is_ascii_whitespace() {
                i += 1;
            }
            chars[v_start..i].iter().collect()
        };
        // Skip trailing whitespace before the separator.
        while i < chars.len() && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        // Optional trailing comma.
        if i < chars.len() && chars[i] == ',' {
            i += 1;
        }
        out.push((key, value));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 2. Signing-string builder
// ---------------------------------------------------------------------------

/// Build the canonical signing string per draft-cavage §2.3.
///
/// Each name in `required_headers` contributes one line:
/// - `(request-target)` → `(request-target): <lowercased method> <path_and_query>`.
/// - any other name → `<lowercased name>: <trimmed header value>`. If the
///   header isn't on the request, returns [`SigError::MissingHeader`] --
///   silently treating it as empty would let an attacker truncate the
///   signed portion of the message.
///
/// Lines are joined with `\n`; there is no trailing newline (the draft is
/// explicit about this).
pub fn build_signing_string(
    method: &str,
    path_and_query: &str,
    headers: &http::HeaderMap,
    required_headers: &[String],
) -> Result<String, SigError> {
    let mut lines = Vec::with_capacity(required_headers.len());
    for name in required_headers {
        let lower = name.to_ascii_lowercase();
        if lower == "(request-target)" {
            lines.push(format!(
                "(request-target): {} {}",
                method.to_ascii_lowercase(),
                path_and_query
            ));
        } else {
            let header_name = http::HeaderName::from_bytes(lower.as_bytes())
                .map_err(|_| SigError::MissingHeader(lower.clone()))?;
            let value = headers
                .get(&header_name)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| SigError::MissingHeader(lower.clone()))?;
            lines.push(format!("{}: {}", lower, value.trim()));
        }
    }
    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// 3. Verifier
// ---------------------------------------------------------------------------

/// Verify a draft-cavage HTTP signature against an OpenSSH public key.
///
/// The allowlist below is the only set of algorithms we accept:
/// - `rsa-sha256`, `rsa-sha512` -- PKCS#1 v1.5 padding.
/// - `ecdsa-sha256`, `ecdsa-sha384` -- signature expected in **DER** format
///   on the wire (this matches what node-triton and `libs/triton-auth` emit
///   on the signing side; SSH wire-format signatures are converted to DER
///   before base64-encoding).
/// - `ed25519` -- raw 64-byte signature.
///
/// Anything else maps to [`SigError::UnsupportedAlgorithm`]. Legacy
/// `rsa-sha1` and HMAC flavours are intentionally off.
pub fn verify_signature(
    public_key: &ssh_key::PublicKey,
    algorithm: &str,
    signing_string: &[u8],
    signature: &[u8],
) -> Result<(), SigError> {
    let key_type = key_type_str(public_key);
    match algorithm {
        "rsa-sha256" => {
            let rsa_pub = require_rsa(public_key.key_data(), algorithm, key_type)?;
            let verifying_key = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(rsa_pub);
            let sig = rsa::pkcs1v15::Signature::try_from(signature)
                .map_err(|_| SigError::VerificationFailed)?;
            verifying_key
                .verify(signing_string, &sig)
                .map_err(|_| SigError::VerificationFailed)
        }
        "rsa-sha512" => {
            let rsa_pub = require_rsa(public_key.key_data(), algorithm, key_type)?;
            let verifying_key = rsa::pkcs1v15::VerifyingKey::<sha2::Sha512>::new(rsa_pub);
            let sig = rsa::pkcs1v15::Signature::try_from(signature)
                .map_err(|_| SigError::VerificationFailed)?;
            verifying_key
                .verify(signing_string, &sig)
                .map_err(|_| SigError::VerificationFailed)
        }
        "ecdsa-sha256" => {
            let ecdsa_pub = require_ecdsa(public_key.key_data(), algorithm, key_type)?;
            let verifying_key: p256::ecdsa::VerifyingKey =
                ecdsa_pub.try_into().map_err(|e: ssh_key::Error| {
                    // Wrong curve (e.g. P-384 key but algorithm said
                    // ecdsa-sha256) shows up here as an ssh-key conversion
                    // failure; surface it as an algorithm/key mismatch so
                    // the caller's 400 story stays consistent.
                    SigError::AlgorithmKeyMismatch {
                        algorithm: algorithm.to_string(),
                        key_type: format!("{key_type} ({e})"),
                    }
                })?;
            let sig = p256::ecdsa::DerSignature::try_from(signature)
                .map_err(|_| SigError::VerificationFailed)?;
            verifying_key
                .verify(signing_string, &sig)
                .map_err(|_| SigError::VerificationFailed)
        }
        "ecdsa-sha384" => {
            let ecdsa_pub = require_ecdsa(public_key.key_data(), algorithm, key_type)?;
            let verifying_key: p384::ecdsa::VerifyingKey =
                ecdsa_pub.try_into().map_err(|e: ssh_key::Error| {
                    SigError::AlgorithmKeyMismatch {
                        algorithm: algorithm.to_string(),
                        key_type: format!("{key_type} ({e})"),
                    }
                })?;
            let sig = p384::ecdsa::DerSignature::try_from(signature)
                .map_err(|_| SigError::VerificationFailed)?;
            verifying_key
                .verify(signing_string, &sig)
                .map_err(|_| SigError::VerificationFailed)
        }
        "ed25519" => {
            let KeyData::Ed25519(ed_pub) = public_key.key_data() else {
                return Err(SigError::AlgorithmKeyMismatch {
                    algorithm: algorithm.to_string(),
                    key_type: key_type.to_string(),
                });
            };
            let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&ed_pub.0).map_err(
                |e: ed25519_dalek::SignatureError| SigError::KeyParseError(e.to_string()),
            )?;
            let sig_bytes: &[u8; 64] = signature
                .try_into()
                .map_err(|_| SigError::VerificationFailed)?;
            let sig = ed25519_dalek::Signature::from_bytes(sig_bytes);
            verifying_key
                .verify(signing_string, &sig)
                .map_err(|_| SigError::VerificationFailed)
        }
        other => Err(SigError::UnsupportedAlgorithm(other.to_string())),
    }
}

/// Short stable string used in `AlgorithmKeyMismatch` errors.
fn key_type_str(public_key: &ssh_key::PublicKey) -> &'static str {
    match public_key.key_data() {
        KeyData::Rsa(_) => "rsa",
        KeyData::Ecdsa(_) => "ecdsa",
        KeyData::Ed25519(_) => "ed25519",
        _ => "other",
    }
}

fn require_rsa(
    data: &KeyData,
    algorithm: &str,
    key_type: &str,
) -> Result<rsa::RsaPublicKey, SigError> {
    match data {
        KeyData::Rsa(rsa_pub) => rsa::RsaPublicKey::try_from(rsa_pub)
            .map_err(|e| SigError::KeyParseError(format!("rsa conversion: {e}"))),
        _ => Err(SigError::AlgorithmKeyMismatch {
            algorithm: algorithm.to_string(),
            key_type: key_type.to_string(),
        }),
    }
}

fn require_ecdsa<'a>(
    data: &'a KeyData,
    algorithm: &str,
    key_type: &str,
) -> Result<&'a ssh_key::public::EcdsaPublicKey, SigError> {
    match data {
        KeyData::Ecdsa(ec) => Ok(ec),
        _ => Err(SigError::AlgorithmKeyMismatch {
            algorithm: algorithm.to_string(),
            key_type: key_type.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! Parser tests stand alone; verifier tests build ssh_key::PublicKey
    //! values out of freshly generated private-key material, sign the
    //! canonical cloudapi signing string with the matching raw crypto
    //! crate (`rsa`, `p256`, `p384`, `ed25519_dalek`), then feed the
    //! signature bytes through `verify_signature`. The sign and verify
    //! halves use the same wire formats cloudapi and node-triton do
    //! (PKCS#1 v1.5 for RSA, DER for ECDSA, raw 64 bytes for Ed25519),
    //! so a green round-trip here is the same wire-format compatibility
    //! check `libs/triton-auth::sign_with_key` would give us if it
    //! worked against ssh-key 0.6.7 -- see the commit message for the
    //! ssh-key-specific reason we don't call it here.
    //!
    //! No private-key material is checked in.
    use super::*;
    use http::{HeaderMap, HeaderValue, header};
    use rand::rngs::OsRng;
    use rsa::pkcs1v15::SigningKey as RsaSigningKey;
    use rsa::signature::{RandomizedSigner, SignatureEncoding};

    // --- Parser tests ------------------------------------------------------

    #[test]
    fn parser_round_trips_a_well_formed_value() {
        let value = r#"keyId="/alice/keys/fp",algorithm="rsa-sha256",headers="date (request-target)",signature="YWJj""#;
        let parsed = parse_signature_params(value).expect("parses");
        assert_eq!(parsed.key_id, "/alice/keys/fp");
        assert_eq!(parsed.algorithm, "rsa-sha256");
        assert_eq!(parsed.headers, vec!["date", "(request-target)"]);
        assert_eq!(parsed.signature, b"abc");
    }

    #[test]
    fn parser_tolerates_whitespace_around_separators() {
        let value = r#"keyId = "/alice/keys/fp" , algorithm = "rsa-sha256" , headers = "date" , signature = "YWJj""#;
        let parsed = parse_signature_params(value).expect("parses");
        assert_eq!(parsed.key_id, "/alice/keys/fp");
        assert_eq!(parsed.algorithm, "rsa-sha256");
        assert_eq!(parsed.headers, vec!["date"]);
    }

    #[test]
    fn parser_accepts_unquoted_values() {
        // draft-cavage permits unquoted values for simple tokens.
        let value = "keyId=/alice/keys/fp,algorithm=rsa-sha256,signature=YWJj";
        let parsed = parse_signature_params(value).expect("parses");
        assert_eq!(parsed.key_id, "/alice/keys/fp");
        assert_eq!(parsed.algorithm, "rsa-sha256");
    }

    #[test]
    fn parser_defaults_headers_to_date_when_missing() {
        // Per draft-cavage, missing `headers=` means only `Date` is signed.
        let value = r#"keyId="/alice/keys/fp",algorithm="rsa-sha256",signature="YWJj""#;
        let parsed = parse_signature_params(value).expect("parses");
        assert_eq!(parsed.headers, vec!["date"]);
    }

    #[test]
    fn parser_rejects_missing_algorithm() {
        let value = r#"keyId="/alice/keys/fp",signature="YWJj""#;
        let err = parse_signature_params(value).expect_err("missing algorithm rejected");
        assert!(matches!(err, SigError::Malformed(_)));
    }

    #[test]
    fn parser_rejects_missing_signature() {
        let value = r#"keyId="/alice/keys/fp",algorithm="rsa-sha256""#;
        let err = parse_signature_params(value).expect_err("missing signature rejected");
        assert!(matches!(err, SigError::Malformed(_)));
    }

    #[test]
    fn parser_rejects_missing_key_id() {
        let value = r#"algorithm="rsa-sha256",signature="YWJj""#;
        let err = parse_signature_params(value).expect_err("missing keyId rejected");
        assert!(matches!(err, SigError::Malformed(_)));
    }

    #[test]
    fn parser_rejects_unbalanced_quotes() {
        let value = r#"keyId="/alice/keys/fp,algorithm="rsa-sha256",signature="YWJj""#;
        let err = parse_signature_params(value).expect_err("unbalanced quotes rejected");
        assert!(matches!(err, SigError::Malformed(_)));
    }

    #[test]
    fn parser_rejects_repeated_key_id() {
        let value = r#"keyId="/alice/keys/fp",keyId="/bob/keys/fp",algorithm="rsa-sha256",signature="YWJj""#;
        let err = parse_signature_params(value).expect_err("duplicate keyId rejected");
        assert!(matches!(err, SigError::Malformed(_)));
    }

    #[test]
    fn parser_ignores_unknown_keys_for_forward_compat() {
        let value =
            r#"keyId="/alice/keys/fp",algorithm="rsa-sha256",created=1700000000,signature="YWJj""#;
        let parsed = parse_signature_params(value).expect("unknown params ignored");
        assert_eq!(parsed.algorithm, "rsa-sha256");
    }

    #[test]
    fn parser_rejects_invalid_base64_signature() {
        let value = r#"keyId="/alice/keys/fp",algorithm="rsa-sha256",signature="!!not-base64!!""#;
        let err = parse_signature_params(value).expect_err("bad base64 rejected");
        assert!(matches!(err, SigError::Malformed(_)));
    }

    // --- Signing-string tests ----------------------------------------------

    #[test]
    fn build_signing_string_covers_request_target_and_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::DATE,
            HeaderValue::from_static("Mon, 15 Dec 2025 10:30:00 GMT"),
        );
        let required = vec!["date".to_string(), "(request-target)".to_string()];
        let got =
            build_signing_string("POST", "/v1/auth/login-ssh", &headers, &required).expect("built");
        assert_eq!(
            got,
            "date: Mon, 15 Dec 2025 10:30:00 GMT\n\
             (request-target): post /v1/auth/login-ssh"
        );
    }

    #[test]
    fn build_signing_string_lowercases_method() {
        let headers = HeaderMap::new();
        let got = build_signing_string(
            "GET",
            "/whatever",
            &headers,
            &["(request-target)".to_string()],
        )
        .expect("built");
        assert_eq!(got, "(request-target): get /whatever");
    }

    #[test]
    fn build_signing_string_fails_on_missing_header() {
        let headers = HeaderMap::new();
        let err = build_signing_string("POST", "/x", &headers, &["date".to_string()])
            .expect_err("missing header rejected");
        assert!(matches!(err, SigError::MissingHeader(ref name) if name == "date"));
    }

    // --- Round-trip verifier tests -----------------------------------------

    /// Build the canonical node-triton signing string (`date: ...\n(request-target): ...`)
    /// and the header list that matches `libs/triton-auth::RequestSigner`.
    fn canonical_signing_string(date: &str, method: &str, path: &str) -> (String, Vec<String>) {
        (
            format!(
                "date: {date}\n(request-target): {} {path}",
                method.to_ascii_lowercase()
            ),
            vec!["date".to_string(), "(request-target)".to_string()],
        )
    }

    fn headers_with_date(date: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::DATE, HeaderValue::from_str(date).expect("ascii"));
        h
    }

    // Test keypairs: we generate with the raw crypto crates and then
    // derive the `ssh_key::PublicKey` our verifier accepts. We don't use
    // `ssh_key::PrivateKey::random`+`sign_with_key` because ssh-key
    // 0.6.7 rejects the empty-namespace call that triton-auth uses and
    // its `RsaKeypair -> rsa::RsaPrivateKey` conversion has an upstream
    // bug. Signing via the raw crates exercises the exact wire format
    // node-triton and cloudapi use (PKCS#1 v1.5 for RSA, DER for
    // ECDSA, raw 64 bytes for Ed25519).

    struct RsaTest {
        sk: rsa::RsaPrivateKey,
        pk: ssh_key::PublicKey,
    }

    struct EcdsaP256Test {
        sk: p256::ecdsa::SigningKey,
        pk: ssh_key::PublicKey,
    }

    struct EcdsaP384Test {
        sk: p384::ecdsa::SigningKey,
        pk: ssh_key::PublicKey,
    }

    struct Ed25519Test {
        sk: ed25519_dalek::SigningKey,
        pk: ssh_key::PublicKey,
    }

    /// 2048-bit RSA is the smallest modulus `ssh_key::RsaPublicKey`
    /// will accept (matches the MIN_KEY_SIZE guard production keys
    /// in mahi satisfy).
    fn gen_rsa() -> RsaTest {
        let sk = rsa::RsaPrivateKey::new(&mut OsRng, 2048).expect("rsa keygen");
        let ssh_pub =
            ssh_key::public::RsaPublicKey::try_from(sk.to_public_key()).expect("rsa ssh pub");
        let pk = ssh_key::PublicKey::new(ssh_key::public::KeyData::from(ssh_pub), "test");
        RsaTest { sk, pk }
    }

    fn gen_ecdsa_p256() -> EcdsaP256Test {
        let sk = p256::ecdsa::SigningKey::random(&mut OsRng);
        let vk = p256::ecdsa::VerifyingKey::from(&sk);
        let ssh_pub = ssh_key::public::EcdsaPublicKey::from(vk);
        let pk = ssh_key::PublicKey::new(ssh_key::public::KeyData::from(ssh_pub), "test");
        EcdsaP256Test { sk, pk }
    }

    fn gen_ecdsa_p384() -> EcdsaP384Test {
        let sk = p384::ecdsa::SigningKey::random(&mut OsRng);
        let vk = p384::ecdsa::VerifyingKey::from(&sk);
        let ssh_pub = ssh_key::public::EcdsaPublicKey::from(vk);
        let pk = ssh_key::PublicKey::new(ssh_key::public::KeyData::from(ssh_pub), "test");
        EcdsaP384Test { sk, pk }
    }

    fn gen_ed25519() -> Ed25519Test {
        let sk = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let vk = sk.verifying_key();
        let ssh_pub = ssh_key::public::Ed25519PublicKey::from(vk);
        let pk = ssh_key::PublicKey::new(ssh_key::public::KeyData::from(ssh_pub), "test");
        Ed25519Test { sk, pk }
    }

    /// Sign `msg` with PKCS#1 v1.5 padding over SHA-256 or SHA-512 and
    /// return the raw modulus-width bytes (no DER wrapping) -- the
    /// shape cloudapi and our verifier both expect.
    fn rsa_sign(sk: &rsa::RsaPrivateKey, hash: &'static str, msg: &[u8]) -> Vec<u8> {
        match hash {
            "sha256" => {
                let signing_key = RsaSigningKey::<sha2::Sha256>::new(sk.clone());
                signing_key
                    .sign_with_rng(&mut OsRng, msg)
                    .to_bytes()
                    .to_vec()
            }
            "sha512" => {
                let signing_key = RsaSigningKey::<sha2::Sha512>::new(sk.clone());
                signing_key
                    .sign_with_rng(&mut OsRng, msg)
                    .to_bytes()
                    .to_vec()
            }
            other => panic!("unknown rsa hash {other}"),
        }
    }

    fn ecdsa_p256_sign(sk: &p256::ecdsa::SigningKey, msg: &[u8]) -> Vec<u8> {
        use p256::ecdsa::signature::Signer;
        let sig: p256::ecdsa::Signature = sk.sign(msg);
        sig.to_der().as_bytes().to_vec()
    }

    fn ecdsa_p384_sign(sk: &p384::ecdsa::SigningKey, msg: &[u8]) -> Vec<u8> {
        use p384::ecdsa::signature::Signer;
        let sig: p384::ecdsa::Signature = sk.sign(msg);
        sig.to_der().as_bytes().to_vec()
    }

    fn ed25519_sign(sk: &ed25519_dalek::SigningKey, msg: &[u8]) -> Vec<u8> {
        use ed25519_dalek::Signer;
        sk.sign(msg).to_bytes().to_vec()
    }

    #[test]
    fn round_trip_rsa_sha256() {
        let key = gen_rsa();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (expected, header_list) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let headers = headers_with_date(date);
        let built = build_signing_string("POST", "/v1/auth/login-ssh", &headers, &header_list)
            .expect("build");
        assert_eq!(built, expected);
        let sig = rsa_sign(&key.sk, "sha256", built.as_bytes());
        verify_signature(&key.pk, "rsa-sha256", built.as_bytes(), &sig)
            .expect("rsa-sha256 round-trip");
    }

    #[test]
    fn round_trip_rsa_sha512() {
        let key = gen_rsa();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (expected, header_list) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let headers = headers_with_date(date);
        let built = build_signing_string("POST", "/v1/auth/login-ssh", &headers, &header_list)
            .expect("build");
        assert_eq!(built, expected);
        let sig = rsa_sign(&key.sk, "sha512", built.as_bytes());
        verify_signature(&key.pk, "rsa-sha512", built.as_bytes(), &sig)
            .expect("rsa-sha512 round-trip");
    }

    #[test]
    fn round_trip_ecdsa_p256() {
        // This is the hardest path: the signature is DER-wrapped on the
        // wire, so `p256::ecdsa::Signature::to_der` on the sign side and
        // `DerSignature::try_from` on the verify side must agree. A
        // regression that slipped to fixed-size `(r||s)` encoding would
        // fail this test loudly.
        let key = gen_ecdsa_p256();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (expected, header_list) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let headers = headers_with_date(date);
        let built = build_signing_string("POST", "/v1/auth/login-ssh", &headers, &header_list)
            .expect("build");
        assert_eq!(built, expected);
        let sig = ecdsa_p256_sign(&key.sk, built.as_bytes());
        verify_signature(&key.pk, "ecdsa-sha256", built.as_bytes(), &sig)
            .expect("ecdsa-sha256 round-trip");
    }

    #[test]
    fn round_trip_ecdsa_p384() {
        let key = gen_ecdsa_p384();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (expected, header_list) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let headers = headers_with_date(date);
        let built = build_signing_string("POST", "/v1/auth/login-ssh", &headers, &header_list)
            .expect("build");
        assert_eq!(built, expected);
        let sig = ecdsa_p384_sign(&key.sk, built.as_bytes());
        verify_signature(&key.pk, "ecdsa-sha384", built.as_bytes(), &sig)
            .expect("ecdsa-sha384 round-trip");
    }

    #[test]
    fn round_trip_ed25519() {
        let key = gen_ed25519();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (expected, header_list) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let headers = headers_with_date(date);
        let built = build_signing_string("POST", "/v1/auth/login-ssh", &headers, &header_list)
            .expect("build");
        assert_eq!(built, expected);
        let sig = ed25519_sign(&key.sk, built.as_bytes());
        verify_signature(&key.pk, "ed25519", built.as_bytes(), &sig).expect("ed25519 round-trip");
    }

    #[test]
    fn tampered_signing_string_fails() {
        let key = gen_rsa();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (good_string, _) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let sig = rsa_sign(&key.sk, "sha256", good_string.as_bytes());
        let tampered = good_string.replace("post /v1/auth/login-ssh", "get /v1/auth/login-ssh");
        let err = verify_signature(&key.pk, "rsa-sha256", tampered.as_bytes(), &sig)
            .expect_err("tamper rejected");
        assert!(matches!(err, SigError::VerificationFailed));
    }

    #[test]
    fn tampered_signature_bytes_fail() {
        let key = gen_ecdsa_p256();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (signing_string, _) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let mut sig = ecdsa_p256_sign(&key.sk, signing_string.as_bytes());
        // Flip a bit near the end of the signature body -- inside an
        // INTEGER value, past the outer DER framing.
        let idx = sig.len() - 3;
        sig[idx] ^= 0x01;
        let err = verify_signature(&key.pk, "ecdsa-sha256", signing_string.as_bytes(), &sig)
            .expect_err("bit-flip rejected");
        assert!(matches!(err, SigError::VerificationFailed));
    }

    #[test]
    fn wrong_public_key_fails() {
        let key_a = gen_ed25519();
        let key_b = gen_ed25519();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (signing_string, _) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        let sig = ed25519_sign(&key_a.sk, signing_string.as_bytes());
        let err = verify_signature(&key_b.pk, "ed25519", signing_string.as_bytes(), &sig)
            .expect_err("wrong key rejected");
        assert!(matches!(err, SigError::VerificationFailed));
    }

    #[test]
    fn algorithm_key_mismatch_rsa_sha256_with_ed25519_key() {
        let key = gen_ed25519();
        let date = "Mon, 15 Dec 2025 10:30:00 GMT";
        let (signing_string, _) = canonical_signing_string(date, "POST", "/v1/auth/login-ssh");
        // Any bytes will do -- we never get to the crypto check.
        let err = verify_signature(
            &key.pk,
            "rsa-sha256",
            signing_string.as_bytes(),
            &[0u8; 128],
        )
        .expect_err("mismatch rejected");
        assert!(matches!(err, SigError::AlgorithmKeyMismatch { .. }));
    }

    #[test]
    fn unsupported_algorithm_hmac_rejected() {
        let key = gen_rsa();
        let err = verify_signature(&key.pk, "hmac-sha256", b"x", b"y").expect_err("hmac rejected");
        assert!(matches!(err, SigError::UnsupportedAlgorithm(ref a) if a == "hmac-sha256"));
    }

    #[test]
    fn unsupported_algorithm_rsa_sha1_rejected() {
        let key = gen_rsa();
        let err = verify_signature(&key.pk, "rsa-sha1", b"x", b"y").expect_err("rsa-sha1 rejected");
        assert!(matches!(err, SigError::UnsupportedAlgorithm(_)));
    }

    #[test]
    fn unsupported_algorithm_empty_rejected() {
        let key = gen_rsa();
        let err = verify_signature(&key.pk, "", b"x", b"y").expect_err("empty rejected");
        assert!(matches!(err, SigError::UnsupportedAlgorithm(_)));
    }

    #[test]
    fn unsupported_algorithm_junk_rejected() {
        let key = gen_rsa();
        let err =
            verify_signature(&key.pk, "not-a-real-alg", b"x", b"y").expect_err("junk rejected");
        assert!(matches!(err, SigError::UnsupportedAlgorithm(_)));
    }
}
