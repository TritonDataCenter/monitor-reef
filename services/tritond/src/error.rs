// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! HTTP-error construction and store/audit/metrics/logs → `HttpError`
//! mappers shared across the handler surface.

use dropshot::{ClientErrorStatusCode, HttpError};
use tritond_audit::Outcome as AuditOutcome;
use tritond_store::StoreError;

/// Map a [`StoreError`] to the appropriate HTTP response.
pub(crate) fn store_error_to_http(err: StoreError) -> HttpError {
    match err {
        StoreError::NotFound => HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            "not found".to_string(),
        ),
        StoreError::Conflict(msg) => HttpError::for_client_error(
            Some("Conflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            msg,
        ),
        StoreError::Backend(msg) => HttpError::for_internal_error(msg),
        // A saga-issued mutation was fenced out by an adopting SEC
        // (RFD 00004 D-Sg-8). Surface as 503 with retry semantics:
        // the operator's request didn't break anything, the
        // adopting SEC is going to drive the saga forward, and the
        // caller can poll `/v2/operations/{id}` to follow it.
        StoreError::FencedOut { saga_id } => HttpError::for_unavail(
            Some("FencedOut".to_string()),
            format!("saga {saga_id} adopted by another tritond instance; retry"),
        ),
        // RFD 00005 PL-2: placement-keyspace errors. PinConflict is
        // operator-visible (409); AlreadyExists is an internal
        // programming error (500); CapacityExhausted is surfaced as
        // 503 with retry semantics so a transient capacity squeeze
        // returns the user-facing "try again" rather than 500.
        StoreError::PinConflict { reason } => HttpError::for_client_error(
            Some("PinConflict".to_string()),
            ClientErrorStatusCode::CONFLICT,
            reason,
        ),
        StoreError::CapacityExhausted {
            server_uuid,
            reason,
        } => HttpError::for_unavail(
            Some("CapacityExhausted".to_string()),
            format!("capacity exhausted on {server_uuid}: {reason}"),
        ),
        StoreError::AlreadyExists(msg) => HttpError::for_internal_error(format!(
            "store reported AlreadyExists from a path that should never collide: {msg}"
        )),
    }
}

pub(crate) fn store_error_to_audit_outcome(err: &StoreError) -> AuditOutcome {
    match err {
        StoreError::NotFound => AuditOutcome::ClientError {
            code: 404,
            message: "not found".to_string(),
        },
        StoreError::Conflict(msg) => AuditOutcome::ClientError {
            code: 409,
            message: msg.clone(),
        },
        StoreError::Backend(msg) => AuditOutcome::ServerError {
            message: msg.clone(),
        },
        StoreError::FencedOut { saga_id } => AuditOutcome::ServerError {
            message: format!("fenced out for saga {saga_id}"),
        },
        StoreError::PinConflict { reason } => AuditOutcome::ClientError {
            code: 409,
            message: reason.clone(),
        },
        StoreError::CapacityExhausted {
            server_uuid,
            reason,
        } => AuditOutcome::ServerError {
            message: format!("cn-capacity exhausted on {server_uuid}: {reason}"),
        },
        StoreError::AlreadyExists(msg) => AuditOutcome::ServerError {
            message: msg.clone(),
        },
    }
}

pub(crate) fn audit_error_to_http(err: tritond_audit::AuditError) -> HttpError {
    use tritond_audit::AuditError;
    let display = err.to_string();
    match err {
        AuditError::PastHead { .. } => HttpError::for_client_error(
            Some("NotFound".to_string()),
            ClientErrorStatusCode::NOT_FOUND,
            display,
        ),
        AuditError::Backend(msg) | AuditError::Serialise(msg) => HttpError::for_internal_error(msg),
        // ChainBroken or any future variant: surface as 500 with the
        // generic display impl so audit-runtime errors don't leak
        // structure-of-the-chain detail to the caller.
        _ => HttpError::for_internal_error(display),
    }
}

pub(crate) fn metrics_error_to_http(e: tritond_metrics::MetricsStoreError) -> HttpError {
    use tritond_metrics::MetricsStoreError as E;
    match e {
        E::InvalidQuery(msg) => HttpError::for_bad_request(None, msg),
        E::UnknownSchema(s) => HttpError::for_bad_request(None, format!("unknown schema: {s}")),
        E::Unavailable(msg) => HttpError::for_unavail(None, msg),
        // `MetricsStoreError` is `#[non_exhaustive]`; future-proof
        // the match so adding a new variant doesn't break this
        // crate at the same time.
        _ => HttpError::for_internal_error(format!("metrics: {e}")),
    }
}

pub(crate) fn logs_error_to_http(e: tritond_logs::LogStoreError) -> HttpError {
    use tritond_logs::LogStoreError as E;
    match e {
        E::InvalidQuery(msg) => HttpError::for_bad_request(None, msg),
        E::Unavailable(msg) => HttpError::for_unavail(None, msg),
        _ => HttpError::for_internal_error(format!("logs: {e}")),
    }
}

/// Generic 404 "not found" used by the defence-in-depth path checks.
/// Same shape as `store_error_to_http` for `StoreError::NotFound`,
/// just inlined so handlers don't have to roll a synthetic StoreError.
pub(crate) fn not_found() -> HttpError {
    HttpError::for_client_error(
        Some("NotFound".to_string()),
        ClientErrorStatusCode::NOT_FOUND,
        "not found".to_string(),
    )
}

pub(crate) fn bad_request(message: impl Into<String>) -> HttpError {
    HttpError::for_bad_request(Some("BadRequest".to_string()), message.into())
}

pub(crate) fn invalid_credentials() -> HttpError {
    HttpError::for_client_error(
        Some("Unauthenticated".to_string()),
        ClientErrorStatusCode::UNAUTHORIZED,
        "invalid credentials".to_string(),
    )
}

/// 429 Too Many Requests with a `Retry-After` header carrying the
/// number of seconds the client should wait before its next attempt.
/// Used by the login rate limiter — see [`crate::rate_limit`].
pub(crate) fn too_many_requests(retry_after: std::time::Duration) -> HttpError {
    // Always at least one second so a client that obeys the header
    // doesn't spin in a tight retry loop.
    let secs = retry_after.as_secs().max(1);
    let mut err = HttpError::for_client_error(
        Some("TooManyRequests".to_string()),
        ClientErrorStatusCode::TOO_MANY_REQUESTS,
        "rate limited; slow down and retry shortly".to_string(),
    );
    let mut headers = http::HeaderMap::new();
    if let Ok(value) = http::HeaderValue::from_str(&secs.to_string()) {
        headers.insert(http::header::RETRY_AFTER, value);
    }
    err.headers = Some(Box::new(headers));
    err
}
