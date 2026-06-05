// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! TOTP (RFC 6238) verification for the password-login 2FA path.
//!
//! Verify-only: this module never generates, stores, or rotates
//! secrets. Enrollment lives in piranha; tritonapi only consumes the
//! secret already written to UFDS metadata under
//! `metadata=portal,uuid=<USER_UUID>,ou=users,o=smartdc` with key
//! `usemoresecurity` (JSON-encoded `{"secretkey": "<base32>"}`).
//!
//! The TOTP parameters below are deliberately fixed and match the
//! piranha defaults so existing enrolments work unchanged: HMAC-SHA-1,
//! 30-second time step, 6-digit code, ±1 step skew window. Loosening
//! any of these would either accept codes piranha rejects (security
//! regression) or reject codes piranha accepts (UX regression).

use crate::error::{SessionError, SessionResult};
use std::time::{SystemTime, UNIX_EPOCH};
use totp_rs::{Algorithm, Secret, TOTP};

const STEP_SECONDS: u64 = 30;
const DIGITS: usize = 6;
/// Accept the immediately-previous and -next time-step in addition to
/// the current one. Covers reasonable client-side clock skew without
/// widening the replay window beyond ~90 seconds.
const SKEW_STEPS: u8 = 1;

/// Verify a TOTP `code` against the supplied base32-encoded `secret`
/// using the current system time.
///
/// Returns `Ok(true)` if the code matches the current step or either
/// neighbour. `Ok(false)` is the user-typed-the-wrong-code case;
/// callers should map that to a generic auth failure. `Err` indicates
/// the stored secret could not be decoded — that is a server-side
/// configuration problem, not a credential failure.
pub fn verify_totp(secret_base32: &str, code: &str) -> SessionResult<bool> {
    verify_totp_at(secret_base32, code, current_unix_time()?)
}

fn verify_totp_at(secret_base32: &str, code: &str, time: u64) -> SessionResult<bool> {
    let totp = build_totp(secret_base32)?;
    Ok(totp.check(code, time))
}

fn build_totp(secret_base32: &str) -> SessionResult<TOTP> {
    let secret_bytes = Secret::Encoded(secret_base32.to_string())
        .to_bytes()
        .map_err(|e| SessionError::Internal(format!("TOTP secret decode failed: {e}")))?;
    TOTP::new(
        Algorithm::SHA1,
        DIGITS,
        SKEW_STEPS,
        STEP_SECONDS,
        secret_bytes,
    )
    .map_err(|e| SessionError::Internal(format!("TOTP init failed: {e}")))
}

fn current_unix_time() -> SessionResult<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| SessionError::Internal(format!("system clock before UNIX epoch: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 §B test secret: ASCII `"12345678901234567890"` (20
    /// bytes), encoded as RFC 4648 base32 (unpadded, uppercase). The
    /// RFC's published 8-digit codes truncate directly to 6 digits by
    /// keeping the low six (HOTP truncation is `% 10^digits`), which
    /// is why the constants below are the RFC values' last 6 digits.
    const RFC6238_SHA1_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn rfc6238_sha1_known_vectors() {
        let vectors = [
            (59u64, "287082"),
            (1111111109, "081804"),
            (1111111111, "050471"),
            (1234567890, "005924"),
            (2000000000, "279037"),
        ];
        for (t, code) in vectors {
            assert!(
                verify_totp_at(RFC6238_SHA1_SECRET, code, t).expect("decode + verify"),
                "RFC 6238 code {code} should be valid at T={t}"
            );
        }
    }

    #[test]
    fn outside_skew_window_rejected() {
        // Generate a code valid at T=60 (step 2) and try it 4 minutes
        // later (step 10) -- well outside the ±1 step window.
        let totp = build_totp(RFC6238_SHA1_SECRET).expect("build totp");
        let stale = totp.generate(60);
        assert!(
            !verify_totp_at(RFC6238_SHA1_SECRET, &stale, 300).expect("verify"),
            "code from 4 minutes ago must not verify"
        );
    }

    #[test]
    fn one_step_skew_accepted_either_side() {
        let totp = build_totp(RFC6238_SHA1_SECRET).expect("build totp");
        let prev_step_code = totp.generate(0); // step 0
        let next_step_code = totp.generate(60); // step 2
        // ±1 around step 1 (T=30..60) -- both should pass.
        assert!(verify_totp_at(RFC6238_SHA1_SECRET, &prev_step_code, 45).expect("verify prev"));
        assert!(verify_totp_at(RFC6238_SHA1_SECRET, &next_step_code, 45).expect("verify next"));
    }

    #[test]
    fn outright_garbage_code_rejected() {
        // Wrong digits, wrong-length, and non-numeric all return
        // false rather than erroring; only the secret being malformed
        // produces an Err.
        assert!(!verify_totp_at(RFC6238_SHA1_SECRET, "000000", 59).expect("verify"));
        assert!(!verify_totp_at(RFC6238_SHA1_SECRET, "abcdef", 59).expect("verify"));
        assert!(!verify_totp_at(RFC6238_SHA1_SECRET, "", 59).expect("verify"));
    }

    #[test]
    fn malformed_secret_returns_internal_error() {
        let err = verify_totp_at("this is not base32!!!", "123456", 59)
            .expect_err("non-base32 secret must surface as Err");
        assert!(
            matches!(err, SessionError::Internal(_)),
            "expected Internal, got {err:?}"
        );
    }
}
