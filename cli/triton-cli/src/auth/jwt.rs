// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Minimal JWT payload decoder for reading the `exp` claim.
//!
//! The gateway already verified the signature when it issued the token;
//! here we only need to know when the access token expires so the CLI
//! can schedule refresh. Pulling in a full `jsonwebtoken` crate just to
//! read one integer claim would be overkill, so this module does the
//! base64url + JSON decoding directly (~20 lines of logic).

use anyhow::{Context as _, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeZone as _, Utc};
use serde::Deserialize;

#[derive(Deserialize)]
struct ExpClaim {
    exp: i64,
}

/// Read the `exp` claim (seconds since epoch) from an access token and
/// return it as a [`DateTime<Utc>`].
///
/// Returns an error for any decoding failure — malformed tokens, bad
/// base64, non-integer `exp`, etc. — so callers can surface a clear
/// "corrupt token file, re-login" message rather than a panic.
pub fn extract_exp(access_token: &str) -> anyhow::Result<DateTime<Utc>> {
    // A compact JWS has exactly three segments separated by '.'.
    let mut parts = access_token.split('.');
    let _header = parts
        .next()
        .ok_or_else(|| anyhow!("jwt missing header segment"))?;
    let payload = parts
        .next()
        .ok_or_else(|| anyhow!("jwt missing payload segment"))?;
    let _signature = parts
        .next()
        .ok_or_else(|| anyhow!("jwt missing signature segment"))?;
    if parts.next().is_some() {
        return Err(anyhow!("jwt has too many segments (want exactly 3)"));
    }

    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .context("base64-decoding jwt payload")?;
    let claim: ExpClaim =
        serde_json::from_slice(&decoded).context("parsing jwt payload as JSON")?;

    Utc.timestamp_opt(claim.exp, 0).single().ok_or_else(|| {
        anyhow!(
            "jwt exp claim {} is not representable as a UTC timestamp",
            claim.exp
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a minimal fake JWT (header.payload.sig) with the given
    /// `exp` value. Signature is junk — we don't verify.
    fn make_fake_jwt(exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","typ":"JWT"}"#);
        let payload_json = serde_json::json!({ "exp": exp, "sub": "someone" }).to_string();
        let payload = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(b"not-a-real-signature");
        format!("{header}.{payload}.{signature}")
    }

    #[test]
    fn extracts_future_exp() {
        let jwt = make_fake_jwt(2_000_000_000); // year 2033
        let exp = extract_exp(&jwt).expect("decode succeeds");
        assert_eq!(exp.timestamp(), 2_000_000_000);
    }

    #[test]
    fn extracts_past_exp() {
        let jwt = make_fake_jwt(1_500_000_000); // year 2017
        let exp = extract_exp(&jwt).expect("decode succeeds");
        assert_eq!(exp.timestamp(), 1_500_000_000);
    }

    #[test]
    fn rejects_wrong_segment_count() {
        assert!(extract_exp("only.two").is_err());
        assert!(extract_exp("a.b.c.d").is_err());
        assert!(extract_exp("").is_err());
    }

    #[test]
    fn rejects_bad_base64_payload() {
        let jwt = "header.!!!not-base64!!!.sig";
        let err = extract_exp(jwt).unwrap_err().to_string();
        assert!(
            err.contains("base64") || err.contains("payload"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_payload_without_exp() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"x"}"#);
        let sig = URL_SAFE_NO_PAD.encode(b"sig");
        let jwt = format!("{header}.{payload}.{sig}");
        assert!(extract_exp(&jwt).is_err());
    }
}
