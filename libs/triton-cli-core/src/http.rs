// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! SmartOS-safe HTTPS client.
//!
//! The illumos global zone ships no system CA bundle, so reqwest's
//! default platform verifier panics at startup ("No CA certificates
//! were loaded from the system"). We bundle the Mozilla `webpki-roots`
//! trust store and preconfigure rustls with it instead — consistent
//! across mac / linux / illumos. Same posture as tritonagent.

use anyhow::{Context, Result};

/// Build a `reqwest::Client` with the bundled webpki-roots trust store.
///
/// When `bearer` is set, an `Authorization: Bearer …` header is attached
/// as a default on every request the returned client makes.
pub fn build_http_client(bearer: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(bearer) = bearer {
        let value = format!("Bearer {bearer}")
            .parse()
            .context("invalid bearer token characters")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    reqwest::Client::builder()
        .default_headers(headers)
        .use_preconfigured_tls(tls)
        .build()
        .context("build reqwest client")
}
