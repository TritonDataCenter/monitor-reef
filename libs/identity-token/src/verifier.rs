// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Verifies an identityd access token against a realm's JWKS.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;

use crate::claims::AccessClaims;
use crate::error::{TokenError, map_validation_err};
use crate::jwks::JwksSource;

/// Asymmetric algorithms we accept. Symmetric (`HS*`) is excluded so a
/// token signed with a public key as an HMAC secret cannot be passed
/// off as valid (the classic RS/HS "alg confusion" attack).
const ALLOWED_ALGS: [Algorithm; 8] = [
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
];

/// What to enforce during verification.
#[derive(Debug, Clone)]
pub struct VerifierOptions {
    /// Required `iss`: the realm's issuer URL.
    pub issuer: String,
    /// Required `aud`, when this resource server scopes by audience.
    pub audience: Option<String>,
    /// Clock-skew leeway in seconds for `exp` / `nbf`.
    pub leeway_secs: u64,
}

impl VerifierOptions {
    /// Defaults: no audience check, 60s leeway.
    #[must_use]
    pub fn new(issuer: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
            audience: None,
            leeway_secs: 60,
        }
    }

    /// Require a specific audience.
    #[must_use]
    pub fn with_audience(mut self, aud: impl Into<String>) -> Self {
        self.audience = Some(aud.into());
        self
    }

    /// Override the clock-skew leeway.
    #[must_use]
    pub fn with_leeway(mut self, secs: u64) -> Self {
        self.leeway_secs = secs;
        self
    }
}

/// Verifies tokens for one issuer against a [`JwksSource`].
pub struct Verifier<S: JwksSource> {
    source: S,
    opts: VerifierOptions,
}

impl<S: JwksSource> Verifier<S> {
    #[must_use]
    pub fn new(source: S, opts: VerifierOptions) -> Self {
        Self { source, opts }
    }

    /// Verify `token` and return its claims. Resolves the signing key by
    /// `kid` (one JWKS refresh on a miss); no other network call.
    pub async fn verify(&self, token: &str) -> Result<AccessClaims, TokenError> {
        let header = decode_header(token).map_err(|e| TokenError::Malformed(e.to_string()))?;
        if !ALLOWED_ALGS.contains(&header.alg) {
            return Err(TokenError::UnsupportedAlg(header.alg));
        }
        let kid = header
            .kid
            .ok_or_else(|| TokenError::Malformed("missing kid header".to_string()))?;
        let jwk = self
            .source
            .jwk_for_kid(&kid)
            .await
            .map_err(|e| TokenError::Jwks(e.to_string()))?
            .ok_or(TokenError::UnknownKid(kid))?;
        let key =
            DecodingKey::from_jwk(&jwk).map_err(|e| TokenError::Jwks(format!("from_jwk: {e}")))?;

        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[self.opts.issuer.as_str()]);
        // A token must not be honored before its `nbf`. jsonwebtoken
        // leaves this off by default; turn it on so a future-dated token
        // is rejected (within `leeway`) instead of silently accepted.
        validation.validate_nbf = true;
        // TODO(RFD 00021): per-resource-server audience. identityd mints
        // `aud: None` today, so audience scoping is not yet enforceable;
        // when a resource-server audience design lands, require it here.
        match &self.opts.audience {
            Some(aud) => validation.set_audience(&[aud.as_str()]),
            None => validation.validate_aud = false,
        }
        validation.leeway = self.opts.leeway_secs;

        let data =
            decode::<AccessClaims>(token, &key, &validation).map_err(map_validation_err)?;
        Ok(data.claims)
    }
}

/// Read the `iss` claim without verifying the signature. Used to route
/// `iss -> realm` before the (verifying) JWKS lookup. Never trust the
/// result for authorization; it is unauthenticated.
pub fn peek_issuer(token: &str) -> Result<String, TokenError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| TokenError::Malformed("token has no payload segment".to_string()))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| TokenError::Malformed(format!("payload base64: {e}")))?;
    #[derive(Deserialize)]
    struct IssOnly {
        iss: String,
    }
    let parsed: IssOnly = serde_json::from_slice(&bytes)
        .map_err(|e| TokenError::Malformed(format!("payload json: {e}")))?;
    Ok(parsed.iss)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::claims::RealmScope;
    use crate::jwks::StaticJwksSource;
    use chrono::Utc;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use uuid::Uuid;

    const ISS: &str = "https://identity.dc-stl-1.example/realms/edge";
    const KID: &str = "test-rsa-1";

    // Throwaway 2048-bit RSA key generated for tests only.
    const TEST_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDJo50Ja4a8obWe
6+rhGJKCuYOFB/FkX2FoCpMYwxEd4ln6f9xhf5QIXR9gzv5PLzt+GLQ59sauWRJ9
oxEnZvdZk2HeljzWzsKgYHP1Md25jl2YJ4cUC9aS7w4nQ9cE2BM+siIQFvwyiUON
1VkmqegjWvz6Pa/UbAZEJ+mfzKpJy2mqe68rJx22hpV04lcsC9RG2r1BLMS2sjw3
xkWYC0BDbTXmgJ8wU4b/LatdMSl/LWiHvvO6xm+PGi3tHyTl00PUUUKYQAgYAeNP
BWJNUA9dvE2QlKdyQutKHLP8+Y1YNOGRVgkdBTGc//4D88C+V/t3NLxS7FEqEjya
7gFDvOhjAgMBAAECggEAHv8WRFzxrOyk6UW16R1oZE0VUS1r57Sb2i0J+Lv/1Rq3
H0FphNliTbjW+oBHaq/FkvhEOEbduu55X7PiDq8O5ge4e0mYF6NYGuRI3w/n2D7w
11N4OdYqCZNTFykgFCANMU2b2+JUqYYdSt/ZoQ5sX4b8rZEvBtYGTpLeIJELOmWS
9TMvyhwYWqNQHxlckYEG3bbhTJ+Noq0cprB0EDwlTMPumUrULYSKy+n4PaX5Ep9k
jn1KySAgL76XXwNwzkfPchDt/9+9InQsSj6lPZbKXRrxbvC0PQnVTLlT9CL+Belp
YNITUznpwTS/EPF96SKEM+kCK2Bp3dJE06HT74M1pQKBgQD5Q0l2eLFdA2IyeJ8w
ZDVjpT/mhq5crwGnsM6BAW1Kiqp5jimLKJFH1tSmRaS06DBHdqq4MVBS6JQCe7aN
UDGwYVEjAWxN6vgN5PTTFA0j+zuQxPzo4OD4cDumK9SHBS3739J3V60VN/7r3LM/
ma/5RJyPCnH/DMBD9oiJpb8KrwKBgQDPFs5IECOwmJ8wn6dKb2syi/yeZ9Yn1ji4
653vSUY3QhY9G6lIV5Sz6HvhIgMuZy3wRLroMSYV8gMfs1qlcL6dWY4KWc9X++/o
lrnBxbSbrJE3IBRuHIrdLKAzjYCz0Xjh2bo/4JQRY4FCzBozmWhVYaMQN1lRnPEj
2waBewXajQKBgD+vGU3TeD0zaBtNBh7u+1UCG1lif5nefKXDXn9HRg0NcJCt6Z9M
NhIMqTfyAyrVR8B6aRO2RbdpBKe9w95G5usRchFng6xEpCuom4RyUwkmXwmVyqgV
DGVtB1BlUl9My3rWrIchN298Zv0L3iAZvAQLq5RALu/i6hxzGF9SoYSxAoGBAMHC
vdlROIN4GaI+DyGnJC6UKhXwY8C8QDBNTGViMs9rBzG/2uD0CQ9X2+imkUFuit3/
AL5VliP8X43em1amKcNB0pO+ujDBahQ+SqmSGU165hgk7Nil3gvZQD9cC2qz+J2g
wRIZR5EJgB0GqRFUXPleNFrs3qZs6Ha67NhjSfPVAoGAbdtytUH3ZGugo4odZobh
wqCbyJURSlb2+HWXC6lK/5ZjITWxmyQKsE7EN4+x5G8c+/mbDfwNleOXdPx1CPhu
ClLN7pe6FHrxc/Du16Oh7gGrkWYny23HjDX1WD4fxl43r9Jhul3e1ScZ+umr78s8
ecIyiVEFXLmnf8wLuFt/dBo=
-----END PRIVATE KEY-----
"#;

    const TEST_JWKS: &str = r#"{"keys":[{"kty":"RSA","use":"sig","alg":"RS256","kid":"test-rsa-1","n":"yaOdCWuGvKG1nuvq4RiSgrmDhQfxZF9haAqTGMMRHeJZ-n_cYX-UCF0fYM7-Ty87fhi0OfbGrlkSfaMRJ2b3WZNh3pY81s7CoGBz9THduY5dmCeHFAvWku8OJ0PXBNgTPrIiEBb8MolDjdVZJqnoI1r8-j2v1GwGRCfpn8yqSctpqnuvKycdtoaVdOJXLAvURtq9QSzEtrI8N8ZFmAtAQ2015oCfMFOG_y2rXTEpfy1oh77zusZvjxot7R8k5dND1FFCmEAIGAHjTwViTVAPXbxNkJSnckLrShyz_PmNWDThkVYJHQUxnP_-A_PAvlf7dzS8UuxRKhI8mu4BQ7zoYw","e":"AQAB"}]}"#;

    fn claims(exp_offset_secs: i64) -> AccessClaims {
        let now = Utc::now().timestamp();
        AccessClaims {
            sub: Uuid::from_u128(0x1111),
            iss: ISS.to_string(),
            aud: None,
            exp: now + exp_offset_secs,
            iat: now,
            nbf: None,
            realm: Uuid::from_u128(0x2222),
            realm_scope: RealmScope::Tenant,
            tenant_id: Some(Uuid::from_u128(0x3333)),
            silo_id: Some(Uuid::from_u128(0x4444)),
            is_root: false,
            fleet_admin: false,
            groups: vec!["developers".to_string()],
            scope: Some("openid profile instances:read".to_string()),
            cnf: None,
        }
    }

    fn mint(c: &AccessClaims, alg: Algorithm, kid: &str) -> String {
        let mut header = Header::new(alg);
        header.kid = Some(kid.to_string());
        let key = match alg {
            Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => {
                EncodingKey::from_secret(b"public-key-as-secret")
            }
            _ => EncodingKey::from_rsa_pem(TEST_PEM.as_bytes()).unwrap(),
        };
        encode(&header, c, &key).unwrap()
    }

    fn verifier(issuer: &str) -> Verifier<StaticJwksSource> {
        Verifier::new(
            StaticJwksSource::from_json(TEST_JWKS).unwrap(),
            VerifierOptions::new(issuer),
        )
    }

    #[tokio::test]
    async fn round_trip_carries_tenant() {
        let token = mint(&claims(3600), Algorithm::RS256, KID);
        let got = verifier(ISS).verify(&token).await.unwrap();
        assert_eq!(got.tenant_id, Some(Uuid::from_u128(0x3333)));
        assert_eq!(got.silo_id, Some(Uuid::from_u128(0x4444)));
        assert_eq!(got.realm_scope, RealmScope::Tenant);
        assert!(got.has_scope("instances:read"));
    }

    #[tokio::test]
    async fn expired_is_rejected() {
        let token = mint(&claims(-3600), Algorithm::RS256, KID);
        let err = verifier(ISS).verify(&token).await.unwrap_err();
        assert!(matches!(err, TokenError::Expired), "got {err:?}");
    }

    #[tokio::test]
    async fn wrong_issuer_is_rejected() {
        let token = mint(&claims(3600), Algorithm::RS256, KID);
        let err = verifier("https://identity.example/realms/other")
            .verify(&token)
            .await
            .unwrap_err();
        assert!(matches!(err, TokenError::InvalidIssuer), "got {err:?}");
    }

    #[tokio::test]
    async fn unknown_kid_is_rejected() {
        let token = mint(&claims(3600), Algorithm::RS256, "no-such-kid");
        let err = verifier(ISS).verify(&token).await.unwrap_err();
        assert!(matches!(err, TokenError::UnknownKid(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn hs256_alg_confusion_is_rejected() {
        // A token that claims HS256 but uses our kid must be refused
        // before any key is loaded.
        let token = mint(&claims(3600), Algorithm::HS256, KID);
        let err = verifier(ISS).verify(&token).await.unwrap_err();
        assert!(matches!(err, TokenError::UnsupportedAlg(_)), "got {err:?}");
    }

    #[test]
    fn peek_issuer_reads_iss() {
        let token = mint(&claims(3600), Algorithm::RS256, KID);
        assert_eq!(peek_issuer(&token).unwrap(), ISS);
    }
}
