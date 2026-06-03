// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! The dev signing key.
//!
//! A single throwaway 2048-bit RSA key is embedded at build time. We do
//! NOT generate keys at runtime (that would drag in a second `rand_core`
//! version against the crypto stack), so this module only *parses* the
//! embedded PEM: it derives the public JWK (so the JWKS endpoint and the
//! signature agree by construction) and hands out a `jsonwebtoken`
//! `EncodingKey` for RS256 signing.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::EncodingKey;
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;

use crate::identifiers::SIGNING_KID;

/// Embedded dev RSA private key (PKCS#8 PEM). Dev/test only.
const DEV_SIGNING_KEY_PEM: &str = include_str!("dev_signing_key.pem");

/// The parsed signing material: the `jsonwebtoken` encoding key plus the
/// public JWK derived from the same key.
pub struct SigningMaterial {
    pub encoding_key: EncodingKey,
    pub public_jwk: serde_json::Value,
}

/// Env flag that gates use of the embedded throwaway dev key. Defaults
/// to enabled so zero-config dev "just works"; a production deployment
/// sets it to `0`/`false`/`no` to refuse to sign with the shared dev key
/// (until a real key source is wired up, that turns into a hard error).
const ALLOW_DEV_KEY_ENV: &str = "IDENTITYD_ALLOW_DEV_KEY";

/// Whether the embedded dev key is permitted. True unless the operator
/// explicitly disables it.
fn dev_key_allowed() -> bool {
    match std::env::var(ALLOW_DEV_KEY_ENV) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Parse the embedded key and derive everything the provider needs.
///
/// This signs with a throwaway dev key baked into the binary. It is fine
/// for the zero-config demo but MUST NOT sign production tokens; the
/// loud warning and the [`ALLOW_DEV_KEY_ENV`] gate exist to make that
/// impossible to do by accident.
pub fn load() -> anyhow::Result<SigningMaterial> {
    if !dev_key_allowed() {
        anyhow::bail!(
            "{ALLOW_DEV_KEY_ENV} is disabled but identityd has no non-dev signing key \
             configured; refusing to sign tokens with the embedded throwaway dev key"
        );
    }
    tracing::warn!(
        "identityd is signing with the EMBEDDED THROWAWAY DEV KEY (kid={SIGNING_KID}); \
         this key is public and MUST NEVER sign production tokens. Set {ALLOW_DEV_KEY_ENV}=0 \
         and provide a real signing key for any non-dev deployment."
    );

    let private = RsaPrivateKey::from_pkcs8_pem(DEV_SIGNING_KEY_PEM)
        .map_err(|e| anyhow::anyhow!("parse embedded signing key: {e}"))?;

    let public = private.to_public_key();
    let n = URL_SAFE_NO_PAD.encode(public.n().to_bytes_be());
    let e = URL_SAFE_NO_PAD.encode(public.e().to_bytes_be());

    let public_jwk = serde_json::json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": SIGNING_KID,
        "n": n,
        "e": e,
    });

    let encoding_key = EncodingKey::from_rsa_pem(DEV_SIGNING_KEY_PEM.as_bytes())
        .map_err(|e| anyhow::anyhow!("build encoding key: {e}"))?;

    Ok(SigningMaterial {
        encoding_key,
        public_jwk,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn embedded_key_parses_and_yields_jwk() {
        let mat = load().unwrap();
        let jwk = mat.public_jwk;
        assert_eq!(jwk["kty"], "RSA");
        assert_eq!(jwk["alg"], "RS256");
        assert_eq!(jwk["kid"], SIGNING_KID);
        assert!(jwk["n"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(jwk["e"], "AQAB");
    }
}
