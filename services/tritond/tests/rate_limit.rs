// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for the per-source-IP login rate limiter.
//!
//! Strategy: build a tritond with a deliberately tight quota
//! (`Quota::per_minute(N)` for small N), hammer `/v2/auth/login`
//! with bad credentials N times, and verify that the (N+1)th attempt
//! comes back 429 with a `Retry-After` header — even when the (N+1)th
//! attempt presents the *correct* password. The point is that the
//! limiter fires before bcrypt and before any credential check,
//! so an attacker cannot dance around it by occasionally guessing
//! right.

use std::num::NonZeroU32;
use std::sync::Arc;

use chrono::Utc;
use governor::Quota;
use tritond::audit::AuditService;
use tritond::auth::AuthService;
use tritond::rate_limit::LoginRateLimiter;
use tritond::{ApiContext, start_server_with_context};
use tritond_audit::MemChain;
use tritond_auth::{JwtKey, RedactedString, hash_password};
use tritond_store::{MemStore, Store, User};
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery staple";
const QUOTA_PER_MIN: u32 = 3;

async fn build_rate_limited_server() -> dropshot::HttpServer<ApiContext> {
    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let user = User {
        id: Uuid::new_v4(),
        username: "root".to_string(),
        password_hash: hash_password(&RedactedString::from(PASSWORD))
            .await
            .unwrap(),
        is_root: true,
        created_at: Utc::now(),
        silo_id: None,
        federation: None,
    };
    store.create_user(user).await.unwrap();
    let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
    let audit = Arc::new(AuditService::new(Arc::new(MemChain::new())));
    let limiter = Arc::new(LoginRateLimiter::with_quota(Quota::per_minute(
        NonZeroU32::new(QUOTA_PER_MIN).unwrap(),
    )));
    let context = ApiContext::new(store, auth, audit).with_login_rate_limiter(limiter);
    start_server_with_context("127.0.0.1:0", context)
        .await
        .unwrap()
}

#[tokio::test]
async fn rate_limit_throttles_after_quota_exhausted() {
    let server = build_rate_limited_server().await;
    let bind = server.local_addr();
    let url = format!("http://{bind}/v2/auth/login");
    let http = reqwest::Client::new();

    for i in 0..QUOTA_PER_MIN {
        let resp = http
            .post(&url)
            .json(&serde_json::json!({
                "username": "root",
                "password": "definitely-not-the-password",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            401,
            "attempt {i}: expected 401 within quota",
        );
    }

    // (N+1)th attempt with the *correct* password — the limiter
    // fires before the credential check, so this must still be 429.
    let resp = http
        .post(&url)
        .json(&serde_json::json!({
            "username": "root",
            "password": PASSWORD,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 429, "over-quota must be 429");
    let retry_after = resp
        .headers()
        .get("retry-after")
        .expect("Retry-After header must be set on 429");
    let secs: u64 = retry_after.to_str().unwrap().parse().unwrap();
    assert!(
        (1..=60).contains(&secs),
        "Retry-After should be 1..=60 secs, got {secs}",
    );

    server.close().await.unwrap();
}

#[tokio::test]
async fn rate_limit_audits_throttle_event() {
    use tritond_audit::Outcome;
    let store: Arc<dyn Store> = Arc::new(MemStore::new());
    let user = User {
        id: Uuid::new_v4(),
        username: "root".to_string(),
        password_hash: hash_password(&RedactedString::from(PASSWORD))
            .await
            .unwrap(),
        is_root: true,
        created_at: Utc::now(),
        silo_id: None,
        federation: None,
    };
    store.create_user(user).await.unwrap();
    let auth = Arc::new(AuthService::new(JwtKey::generate()).unwrap());
    let chain: Arc<MemChain> = Arc::new(MemChain::new());
    let audit = Arc::new(AuditService::new(chain.clone()));
    // One attempt allowed per minute — the second triggers the throttle.
    let limiter = Arc::new(LoginRateLimiter::with_quota(Quota::per_minute(
        NonZeroU32::new(1).unwrap(),
    )));
    let context = ApiContext::new(store, auth, audit).with_login_rate_limiter(limiter);
    let server = start_server_with_context("127.0.0.1:0", context)
        .await
        .unwrap();
    let bind = server.local_addr();
    let url = format!("http://{bind}/v2/auth/login");
    let http = reqwest::Client::new();

    // First attempt: in quota, returns 401 (bad password).
    let r1 = http
        .post(&url)
        .json(&serde_json::json!({"username":"root","password":"nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status().as_u16(), 401);

    // Second attempt: throttled (429).
    let r2 = http
        .post(&url)
        .json(&serde_json::json!({"username":"root","password":"nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status().as_u16(), 429);

    // Audit chain should contain a `login` event whose outcome is
    // ClientError { code: 429, .. } — distinct from the bad-password
    // Unauthenticated event so operators can spot brute-force patterns.
    use tritond_audit::Chain as _;
    let events = chain.list(0, 100).await.unwrap();
    let throttle_event = events
        .iter()
        .find(|e| {
            e.action == "login" && matches!(&e.outcome, Outcome::ClientError { code: 429, .. })
        })
        .expect("expected a 429 audit event for the throttled login");
    if let Outcome::ClientError { code, message } = &throttle_event.outcome {
        assert_eq!(*code, 429);
        assert!(message.contains("rate limited"));
    }

    server.close().await.unwrap();
}
