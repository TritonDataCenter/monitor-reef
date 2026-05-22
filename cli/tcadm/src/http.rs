// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! HTTP client builder for the install + self-update paths.
//!
//! reqwest 0.13's `rustls` feature uses `rustls-platform-verifier`,
//! which looks for a system trust store and errors out on illumos
//! with "No CA certificates were loaded from the system." Bundling
//! the Mozilla roots at build time via the `webpki-roots` crate
//! sidesteps the platform-specific trust-store lookup entirely.

use anyhow::{Context, Result};

/// Construct a blocking reqwest Client whose rustls config uses the
/// bundled Mozilla webpki roots. Same trust set on every platform;
/// no dependency on `/etc/ssl/certs/...` or the macOS keychain.
pub fn blocking_client() -> Result<reqwest::blocking::Client> {
    // Default crypto provider — rustls 0.23 requires one to be
    // explicitly installed before building any ClientConfig.
    // `install_default` is idempotent across the process; we ignore
    // its result so a second call (e.g. from a unit test) does not
    // panic.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    // reqwest downcasts the value to rustls::ClientConfig directly,
    // not Arc<ClientConfig>; passing the bare value is what matches.
    reqwest::blocking::Client::builder()
        .use_preconfigured_tls(config)
        .build()
        .context("building blocking reqwest client with bundled webpki roots")
}
