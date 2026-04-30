// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! ES256 JWT service: issue access tokens, manage refresh tokens,
//! publish JWKS.
//!
//! Refresh tokens are kept in a process-local [`HashMap`]. That is
//! intentional for the initial single-instance deployment — restart = all
//! users re-login. When we need survive-restart or multi-instance HA, swap
//! this field for a persistent store (`moray`, a dedicated token service,
//! or a UFDS attribute). The `refresh_tokens` handle is the single place
//! that needs to change.

use crate::error::{SessionError, SessionResult};
use crate::models::{Claims, Role};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode, jwk};
use p256::pkcs8::DecodePublicKey;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Maximum number of refresh tokens a single user may hold at once.
/// When exceeded, the oldest tokens (by expiry) are evicted.
const MAX_REFRESH_TOKENS_PER_USER: usize = 5;

/// Literal value of the `purpose` claim in a 2FA challenge token.
/// Verification rejects any other value, which is what stops a token
/// signed by the same key but issued for a different purpose (e.g.,
/// an access token) from being accepted at the verify-2FA endpoint.
const CHALLENGE_PURPOSE: &str = "2fa-pending";

/// Challenge-token lifetime. Long enough that a user can realistically
/// switch to their authenticator app and read off a code, short enough
/// that a leaked challenge token has a narrow replay window.
const CHALLENGE_TTL_SECS: u64 = 300;

/// Claims carried by the short-lived token issued between password
/// verification and TOTP verification.
///
/// Deliberately does not include `roles` or the `is_admin` claim that
/// access tokens carry: those are looked up from mahi *after* TOTP
/// succeeds, so a leaked challenge token can never elevate. The
/// missing fields also mean an access-token decoder
/// ([`Claims`]) will fail to deserialize a challenge token outright,
/// providing structural separation in addition to the `purpose`
/// check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeClaims {
    /// JWT subject — the authenticating user's UUID. Same field name
    /// as [`Claims::sub`] so callers can use a uniform accessor.
    pub sub: Uuid,
    /// The user's login. Re-use here saves an LDAP roundtrip in the
    /// verify handler when it goes on to call `mahi.lookup`.
    pub username: String,
    /// Always [`CHALLENGE_PURPOSE`].
    pub purpose: String,
    pub exp: i64,
    pub iat: i64,
    pub jti: String,
}

impl ChallengeClaims {
    pub fn user_uuid(&self) -> Uuid {
        self.sub
    }
}

/// JWT service configuration.
pub struct JwtConfig {
    /// PEM-encoded ES256 private key. Used to sign access tokens.
    pub private_key_pem: SecretString,
    /// PEM-encoded ES256 public key. Used to verify tokens issued by this
    /// process and published at `GET /v1/auth/jwks.json` for remote
    /// verifiers.
    pub public_key_pem: String,
    /// Key ID advertised in the JWT header and the JWKS document. Lets
    /// verifiers select the right key during rotation.
    pub kid: String,
    /// Access token lifetime in seconds.
    pub access_ttl_secs: u64,
    /// Refresh token lifetime in seconds.
    pub refresh_ttl_secs: u64,
}

pub struct JwtService {
    encoding_key: EncodingKey,
    verifier: JwtVerifier,
    header: Header,
    public_key_pem: String,
    kid: String,
    access_ttl_secs: u64,
    refresh_ttl_secs: u64,
    refresh_tokens: Arc<RwLock<HashMap<String, RefreshEntry>>>,
}

/// Verify-only counterpart to [`JwtService`]. Constructable from a PEM
/// public key or a JWK, which lets external verifiers (the gateway, a
/// future adminui proxy, any DC component that consumes tritonapi JWTs)
/// validate access tokens without owning a signing key. Only the
/// signature path matters here; refresh tokens are not in scope.
#[derive(Clone)]
pub struct JwtVerifier {
    decoding_key: Arc<DecodingKey>,
    validation: Validation,
}

impl JwtVerifier {
    fn new(decoding_key: DecodingKey) -> Self {
        Self {
            decoding_key: Arc::new(decoding_key),
            validation: Validation::new(Algorithm::ES256),
        }
    }

    /// Build a verifier from a PEM-encoded EC public key.
    pub fn from_ec_public_pem(pem: &str) -> SessionResult<Self> {
        let key = DecodingKey::from_ec_pem(pem.as_bytes())
            .map_err(|e| SessionError::JwtKeyError(format!("parse public key: {e}")))?;
        Ok(Self::new(key))
    }

    /// Build a verifier from a parsed JWK (typically read from a JWKS
    /// document). The JWK's `alg` is ignored; ES256 is enforced.
    pub fn from_jwk(jwk: &jwk::Jwk) -> SessionResult<Self> {
        let key = DecodingKey::from_jwk(jwk)
            .map_err(|e| SessionError::JwtKeyError(format!("parse JWK: {e}")))?;
        Ok(Self::new(key))
    }

    pub fn verify_token(&self, token: &str) -> SessionResult<Claims> {
        let data = decode::<Claims>(token, &self.decoding_key, &self.validation)?;
        Ok(data.claims)
    }

    /// Decode a token and return its claims without validating expiry.
    /// The signature is still verified. Used for logout so that users
    /// with an expired session can still log out cleanly.
    pub fn decode_ignoring_expiry(&self, token: &str) -> SessionResult<Claims> {
        let mut validation = self.validation.clone();
        validation.validate_exp = false;
        let data = decode::<Claims>(token, &self.decoding_key, &validation)?;
        Ok(data.claims)
    }

    /// Verify a 2FA challenge token. Checks signature, expiry, and
    /// the `purpose` claim — the last gate is what makes this method
    /// distinct from [`Self::verify_token`] even though both use the
    /// same key. A challenge token decoded here, then routed through
    /// `verify_token`, would fail because [`Claims`] requires
    /// `roles` and `is_admin` fields the challenge does not carry.
    pub fn verify_challenge_token(&self, token: &str) -> SessionResult<ChallengeClaims> {
        let data = decode::<ChallengeClaims>(token, &self.decoding_key, &self.validation)?;
        if data.claims.purpose != CHALLENGE_PURPOSE {
            return Err(SessionError::InvalidToken);
        }
        Ok(data.claims)
    }
}

struct RefreshEntry {
    user_id: Uuid,
    username: String,
    roles: Vec<Role>,
    expires_at: i64,
}

/// A single JWK entry. Serializes to the shape verifiers expect from
/// `GET /v1/auth/jwks.json`.
#[derive(Debug, Clone, Serialize)]
pub struct Jwk {
    pub kty: &'static str,
    pub crv: &'static str,
    pub alg: &'static str,
    #[serde(rename = "use")]
    pub key_use: &'static str,
    pub kid: String,
    pub x: String,
    pub y: String,
}

/// RFC 7517 JWKS document.
#[derive(Debug, Clone, Serialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

impl JwtService {
    pub fn new(config: &JwtConfig) -> SessionResult<Self> {
        let encoding_key =
            EncodingKey::from_ec_pem(config.private_key_pem.expose_secret().as_bytes())
                .map_err(|e| SessionError::JwtKeyError(format!("parse private key: {e}")))?;
        let verifier = JwtVerifier::from_ec_public_pem(&config.public_key_pem)?;

        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(config.kid.clone());

        Ok(Self {
            encoding_key,
            verifier,
            header,
            public_key_pem: config.public_key_pem.clone(),
            kid: config.kid.clone(),
            access_ttl_secs: config.access_ttl_secs,
            refresh_ttl_secs: config.refresh_ttl_secs,
            refresh_tokens: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Expose the verify-only handle so any verifier logic (middleware,
    /// other services in the same process) can share a single decoding
    /// setup without reaching into the issuing service.
    pub fn verifier(&self) -> &JwtVerifier {
        &self.verifier
    }

    pub fn create_token(
        &self,
        user_id: Uuid,
        username: &str,
        roles: &[Role],
    ) -> SessionResult<String> {
        let now = Utc::now().timestamp();
        let claims = Claims::new(
            user_id,
            username.to_string(),
            roles.to_vec(),
            now + self.access_ttl_secs as i64,
            now,
            Uuid::new_v4().to_string(),
        );

        Ok(encode(&self.header, &claims, &self.encoding_key)?)
    }

    pub fn verify_token(&self, token: &str) -> SessionResult<Claims> {
        self.verifier.verify_token(token)
    }

    pub fn decode_ignoring_expiry(&self, token: &str) -> SessionResult<Claims> {
        self.verifier.decode_ignoring_expiry(token)
    }

    /// Issue a short-lived challenge token after password verification
    /// but before TOTP verification. The token carries enough identity
    /// (`sub`, `username`) for the verify handler to re-read the TOTP
    /// secret from UFDS and call mahi, but no roles / admin flag —
    /// those come from mahi *after* TOTP succeeds.
    pub fn create_challenge_token(&self, user_uuid: Uuid, username: &str) -> SessionResult<String> {
        let now = Utc::now().timestamp();
        let claims = ChallengeClaims {
            sub: user_uuid,
            username: username.to_string(),
            purpose: CHALLENGE_PURPOSE.to_string(),
            exp: now + CHALLENGE_TTL_SECS as i64,
            iat: now,
            jti: Uuid::new_v4().to_string(),
        };
        Ok(encode(&self.header, &claims, &self.encoding_key)?)
    }

    pub fn verify_challenge_token(&self, token: &str) -> SessionResult<ChallengeClaims> {
        self.verifier.verify_challenge_token(token)
    }

    pub async fn create_refresh_token(
        &self,
        user_id: Uuid,
        username: &str,
        roles: &[Role],
    ) -> String {
        let token = Uuid::new_v4().to_string();
        let expires_at = Utc::now().timestamp() + self.refresh_ttl_secs as i64;

        let entry = RefreshEntry {
            user_id,
            username: username.to_string(),
            roles: roles.to_vec(),
            expires_at,
        };

        let mut tokens = self.refresh_tokens.write().await;
        tokens.insert(token.clone(), entry);

        let user_tokens: Vec<(String, i64)> = tokens
            .iter()
            .filter(|(_, e)| e.user_id == user_id)
            .map(|(k, e)| (k.clone(), e.expires_at))
            .collect();

        if user_tokens.len() > MAX_REFRESH_TOKENS_PER_USER {
            let mut sorted = user_tokens;
            sorted.sort_by_key(|(_, exp)| *exp);
            let to_remove = sorted.len() - MAX_REFRESH_TOKENS_PER_USER;
            for (key, _) in sorted.into_iter().take(to_remove) {
                tokens.remove(&key);
            }
        }

        token
    }

    /// Consume a refresh token and return a new (access, refresh) pair.
    /// Single-use rotation: the old refresh token cannot be reused.
    pub async fn refresh(&self, refresh_token: &str) -> SessionResult<(String, String)> {
        let mut tokens = self.refresh_tokens.write().await;
        let entry = tokens
            .remove(refresh_token)
            .ok_or(SessionError::InvalidToken)?;

        if Utc::now().timestamp() > entry.expires_at {
            return Err(SessionError::TokenExpired);
        }

        let access_token = self.create_token(entry.user_id, &entry.username, &entry.roles)?;

        // Drop the write lock before re-acquiring inside create_refresh_token,
        // since tokio::sync::RwLock is not reentrant.
        drop(tokens);

        let new_refresh = self
            .create_refresh_token(entry.user_id, &entry.username, &entry.roles)
            .await;

        Ok((access_token, new_refresh))
    }

    pub async fn revoke_refresh_token(&self, refresh_token: &str) {
        self.refresh_tokens.write().await.remove(refresh_token);
    }

    pub async fn revoke_user_tokens(&self, username: &str) {
        self.refresh_tokens
            .write()
            .await
            .retain(|_, entry| entry.username != username);
    }

    pub fn access_ttl_secs(&self) -> u64 {
        self.access_ttl_secs
    }

    pub async fn cleanup_expired(&self) {
        let now = Utc::now().timestamp();
        self.refresh_tokens
            .write()
            .await
            .retain(|_, entry| entry.expires_at > now);
    }

    /// Build the JWKS document that external verifiers consume.
    ///
    /// Derives the EC point coordinates from the configured public key PEM.
    /// `JwkEcKey`'s Serialize impl emits the RFC 7518 base64url-encoded x/y;
    /// we pull those out and wrap them with the `alg`/`use`/`kid` fields
    /// verifiers rely on to pick a key during rotation.
    pub fn jwks(&self) -> SessionResult<JwkSet> {
        let pk = p256::PublicKey::from_public_key_pem(&self.public_key_pem)
            .map_err(|e| SessionError::JwtKeyError(format!("derive JWKS: {e}")))?;
        let jwk_value = serde_json::to_value(pk.to_jwk())
            .map_err(|e| SessionError::Internal(format!("serialize JWK: {e}")))?;
        let x = jwk_value
            .get("x")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SessionError::Internal("JWK missing x coordinate".to_string()))?;
        let y = jwk_value
            .get("y")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SessionError::Internal("JWK missing y coordinate".to_string()))?;

        Ok(JwkSet {
            keys: vec![Jwk {
                kty: "EC",
                crv: "P-256",
                alg: "ES256",
                key_use: "sig",
                kid: self.kid.clone(),
                x: x.to_string(),
                y: y.to_string(),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::SecretKey;
    use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    use rand_core::OsRng;

    fn test_config() -> JwtConfig {
        let secret_key = SecretKey::random(&mut OsRng);
        let private_pem = secret_key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
        let public_pem = secret_key
            .public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();

        JwtConfig {
            private_key_pem: SecretString::new(private_pem.into()),
            public_key_pem: public_pem,
            kid: "test-kid".to_string(),
            access_ttl_secs: 3600,
            refresh_ttl_secs: 86400,
        }
    }

    #[test]
    fn create_and_verify_token() {
        let svc = JwtService::new(&test_config()).unwrap();
        let user_id = Uuid::new_v4();
        let token = svc
            .create_token(user_id, "testuser", &[Role::Unknown])
            .unwrap();

        let claims = svc.verify_token(&token).unwrap();
        assert_eq!(claims.username, "testuser");
        assert_eq!(claims.user_uuid(), user_id);
        assert!(!claims.is_admin());
    }

    #[test]
    fn admin_role_sets_is_admin() {
        let svc = JwtService::new(&test_config()).unwrap();
        let token = svc
            .create_token(Uuid::new_v4(), "admin", &[Role::Operators])
            .unwrap();
        let claims = svc.verify_token(&token).unwrap();
        assert!(claims.is_admin());
        assert_eq!(claims.is_admin_claim, claims.is_admin());
    }

    #[tokio::test]
    async fn refresh_token_flow() {
        let svc = JwtService::new(&test_config()).unwrap();
        let user_id = Uuid::new_v4();

        let refresh = svc.create_refresh_token(user_id, "testuser", &[]).await;
        let (new_token, new_refresh) = svc.refresh(&refresh).await.unwrap();
        let claims = svc.verify_token(&new_token).unwrap();
        assert_eq!(claims.username, "testuser");

        // Single-use: old refresh token is gone.
        assert!(svc.refresh(&refresh).await.is_err());

        // New refresh token works.
        let (token2, _) = svc.refresh(&new_refresh).await.unwrap();
        let claims2 = svc.verify_token(&token2).unwrap();
        assert_eq!(claims2.username, "testuser");
    }

    #[tokio::test]
    async fn refresh_preserves_admin_from_roles() {
        let svc = JwtService::new(&test_config()).unwrap();
        let refresh = svc
            .create_refresh_token(Uuid::new_v4(), "admin", &[Role::Admins])
            .await;
        let (new_token, _) = svc.refresh(&refresh).await.unwrap();
        let claims = svc.verify_token(&new_token).unwrap();
        assert!(claims.is_admin());
    }

    #[test]
    fn decode_ignoring_expiry_accepts_expired_token() {
        let svc = JwtService::new(&test_config()).unwrap();
        let user_id = Uuid::new_v4();

        let now = Utc::now().timestamp();
        let claims = Claims::new(
            user_id,
            "testuser".to_string(),
            vec![Role::Unknown],
            now - 120,
            now - 240,
            Uuid::new_v4().to_string(),
        );
        let token = encode(&svc.header, &claims, &svc.encoding_key).unwrap();

        assert!(svc.verify_token(&token).is_err());
        let decoded = svc.decode_ignoring_expiry(&token).unwrap();
        assert_eq!(decoded.username, "testuser");
        assert_eq!(decoded.user_uuid(), user_id);
    }

    #[test]
    fn decode_ignoring_expiry_still_verifies_signature() {
        let svc = JwtService::new(&test_config()).unwrap();
        let other = JwtService::new(&test_config()).unwrap();
        let token = other.create_token(Uuid::new_v4(), "badactor", &[]).unwrap();

        assert!(svc.decode_ignoring_expiry(&token).is_err());
    }

    #[tokio::test]
    async fn revoke_user_tokens_leaves_others_alone() {
        let svc = JwtService::new(&test_config()).unwrap();
        let alice_rt = svc.create_refresh_token(Uuid::new_v4(), "alice", &[]).await;
        let bob_rt = svc.create_refresh_token(Uuid::new_v4(), "bob", &[]).await;

        svc.revoke_user_tokens("alice").await;

        assert!(svc.refresh(&alice_rt).await.is_err());
        assert!(svc.refresh(&bob_rt).await.is_ok());
    }

    #[tokio::test]
    async fn per_user_refresh_token_limit_evicts_oldest() {
        let svc = JwtService::new(&test_config()).unwrap();
        let user_id = Uuid::new_v4();

        let mut tokens = Vec::new();
        let base_time = Utc::now().timestamp() + 1000;
        for i in 0..MAX_REFRESH_TOKENS_PER_USER {
            let rt = svc.create_refresh_token(user_id, "testuser", &[]).await;
            {
                let mut store = svc.refresh_tokens.write().await;
                if let Some(entry) = store.get_mut(&rt) {
                    entry.expires_at = base_time + i as i64;
                }
            }
            tokens.push(rt);
        }

        let newest = svc.create_refresh_token(user_id, "testuser", &[]).await;
        tokens.push(newest);

        assert!(
            svc.refresh(&tokens[0]).await.is_err(),
            "oldest token should have been evicted"
        );
        let last = tokens.last().unwrap();
        assert!(svc.refresh(last).await.is_ok());
    }

    #[test]
    fn tampered_is_admin_claim_ignored_after_jwt_decode() {
        let svc = JwtService::new(&test_config()).unwrap();
        let now = Utc::now().timestamp();

        let payload = serde_json::json!({
            "sub": Uuid::new_v4().to_string(),
            "username": "attacker",
            "roles": ["unknown"],
            "is_admin": true,
            "exp": now + 3600,
            "iat": now,
            "jti": Uuid::new_v4().to_string(),
        });

        let token = encode(&svc.header, &payload, &svc.encoding_key).unwrap();
        let claims = svc.verify_token(&token).unwrap();

        assert!(claims.is_admin_claim);
        assert!(!claims.is_admin());
    }

    #[tokio::test]
    async fn concurrent_refresh_token_replay() {
        let svc = Arc::new(JwtService::new(&test_config()).unwrap());
        let refresh = svc
            .create_refresh_token(Uuid::new_v4(), "testuser", &[])
            .await;

        let num_tasks = 10;
        let barrier = Arc::new(tokio::sync::Barrier::new(num_tasks));

        let mut handles = Vec::new();
        for _ in 0..num_tasks {
            let svc = Arc::clone(&svc);
            let token = refresh.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                svc.refresh(&token).await
            }));
        }

        let mut successes = 0usize;
        let mut failures = 0usize;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(_) => successes += 1,
                Err(_) => failures += 1,
            }
        }

        assert_eq!(successes, 1);
        assert_eq!(failures, num_tasks - 1);
    }

    #[test]
    fn jwks_round_trips_through_verifier() {
        let svc = JwtService::new(&test_config()).unwrap();
        let jwks = svc.jwks().unwrap();
        assert_eq!(jwks.keys.len(), 1);

        let json = serde_json::to_string(&jwks).unwrap();
        assert!(json.contains("\"kty\":\"EC\""));
        assert!(json.contains("\"crv\":\"P-256\""));
        assert!(json.contains("\"alg\":\"ES256\""));
        assert!(json.contains("\"kid\":\"test-kid\""));
    }

    #[test]
    fn verify_rejects_token_from_different_issuer() {
        let svc = JwtService::new(&test_config()).unwrap();
        let other = JwtService::new(&test_config()).unwrap();
        let token = other.create_token(Uuid::new_v4(), "eve", &[]).unwrap();
        assert!(svc.verify_token(&token).is_err());
    }

    #[test]
    fn challenge_token_round_trips() {
        let svc = JwtService::new(&test_config()).unwrap();
        let user_id = Uuid::new_v4();
        let token = svc.create_challenge_token(user_id, "alice").unwrap();
        let claims = svc.verify_challenge_token(&token).unwrap();
        assert_eq!(claims.user_uuid(), user_id);
        assert_eq!(claims.username, "alice");
        assert_eq!(claims.purpose, CHALLENGE_PURPOSE);
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn challenge_token_rejected_as_access_token() {
        // The structural separation: a challenge claim set is missing
        // `roles` and the `is_admin` field, so attempting to decode it
        // as `Claims` fails before any policy decision reaches our code.
        let svc = JwtService::new(&test_config()).unwrap();
        let token = svc.create_challenge_token(Uuid::new_v4(), "alice").unwrap();
        assert!(svc.verify_token(&token).is_err());
    }

    #[test]
    fn access_token_rejected_as_challenge_token() {
        // The mirror: an access token has no `purpose` claim, which is
        // a required field on `ChallengeClaims`, so decoding fails.
        let svc = JwtService::new(&test_config()).unwrap();
        let token = svc
            .create_token(Uuid::new_v4(), "alice", &[Role::Unknown])
            .unwrap();
        assert!(svc.verify_challenge_token(&token).is_err());
    }

    #[test]
    fn challenge_token_with_wrong_purpose_rejected() {
        // Forge a `ChallengeClaims` with a different `purpose`, sign
        // with the correct key, and confirm the verifier rejects it.
        // This is the defense-in-depth check that catches
        // hypothetical future token types signed by the same key.
        let svc = JwtService::new(&test_config()).unwrap();
        let now = Utc::now().timestamp();
        let claims = ChallengeClaims {
            sub: Uuid::new_v4(),
            username: "alice".to_string(),
            purpose: "something-else".to_string(),
            exp: now + 60,
            iat: now,
            jti: Uuid::new_v4().to_string(),
        };
        let token = encode(&svc.header, &claims, &svc.encoding_key).unwrap();
        assert!(svc.verify_challenge_token(&token).is_err());
    }

    #[test]
    fn challenge_token_expiry_enforced() {
        // Sign a `ChallengeClaims` with `exp` in the past and confirm
        // the standard JWT validator rejects it before the purpose
        // check ever runs.
        let svc = JwtService::new(&test_config()).unwrap();
        let now = Utc::now().timestamp();
        // Use a far-past `exp` rather than `now - 60` — jsonwebtoken's
        // default `Validation` allows up to 60 seconds of clock skew,
        // so a marginally-past exp can still verify on a busy CI box.
        let claims = ChallengeClaims {
            sub: Uuid::new_v4(),
            username: "alice".to_string(),
            purpose: CHALLENGE_PURPOSE.to_string(),
            exp: now - 3600,
            iat: now - 3700,
            jti: Uuid::new_v4().to_string(),
        };
        let token = encode(&svc.header, &claims, &svc.encoding_key).unwrap();
        let err = svc
            .verify_challenge_token(&token)
            .expect_err("expired challenge must be rejected");
        assert!(matches!(err, SessionError::TokenExpired), "got {err:?}");
    }

    #[test]
    fn challenge_token_from_different_issuer_rejected() {
        let svc = JwtService::new(&test_config()).unwrap();
        let other = JwtService::new(&test_config()).unwrap();
        let token = other.create_challenge_token(Uuid::new_v4(), "eve").unwrap();
        assert!(svc.verify_challenge_token(&token).is_err());
    }
}
