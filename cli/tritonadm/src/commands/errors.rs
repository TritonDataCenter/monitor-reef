// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Helpers for inspecting `progenitor_client::Error` values.
//!
//! We frequently need to distinguish "the resource does not exist" (404)
//! from "the API call itself failed" (5xx, transport errors, etc.). The
//! former is often expected and tolerable; the latter is a real failure
//! that must not be silently downgraded to a default value.

/// Returns `true` if the error came from an HTTP 404 response.
pub(crate) fn is_404<E>(err: &progenitor_client::Error<E>) -> bool {
    err.status().map(|s| s.as_u16()) == Some(404)
}

/// Returns `true` if an `imgapi_client::ActionError` came from an HTTP 404
/// response. The TypedClient wraps both Progenitor `Error<types::Error>`
/// and `Error<ByteStream>` variants, so we have to peek into both.
pub(crate) fn action_is_404(err: &imgapi_client::ActionError) -> bool {
    use imgapi_client::ActionError;
    match err {
        ActionError::Typed(e) => is_404(e),
        ActionError::ByteStream(e) => is_404(e),
        ActionError::Reqwest(e) => e.status().map(|s| s.as_u16()) == Some(404),
        ActionError::Deserialize(_) => false,
    }
}
