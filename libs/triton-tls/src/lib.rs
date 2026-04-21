// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! TLS certificate loading and HTTP client construction for Triton CLI tools.
//!
//! Platforms like SmartOS/illumos store CA certificates in locations that
//! `rustls-native-certs` doesn't probe by default. This crate provides a
//! multi-tier fallback strategy:
//!
//! 1. Native system certs (respects `SSL_CERT_FILE` / `SSL_CERT_DIR`)
//! 2. Extra platform-specific paths (SmartOS pkgsrc, etc.)
//! 3. Bundled Mozilla roots (via `webpki-roots`) as a last resort
//!
//! The crate also owns process-wide installation of the `ring` rustls
//! crypto provider. Because the workspace builds reqwest with
//! `rustls-no-provider` (see the top-level `Cargo.toml` for the five
//! places that must move in lockstep to switch providers), every
//! `reqwest::Client::builder().build()` and every `rustls::*Config::builder()`
//! call will panic with "No provider set" unless a default `CryptoProvider`
//! is installed first. [`build_http_client`] takes care of that for its
//! own callers; binaries or tests that build rustls configs directly
//! should call [`install_default_crypto_provider`] themselves before
//! touching `rustls::ClientConfig::builder()` or `rustls::ServerConfig::builder()`.

use std::sync::Once;

/// Install the `ring` rustls crypto provider as this process's default,
/// exactly once.
///
/// `install_default()` itself is idempotent (the second call returns
/// `Err`, which we discard), but we guard it behind [`std::sync::Once`]
/// so repeated calls from a hot path like [`build_http_client`] don't
/// re-allocate a provider struct just to throw it away.
pub fn install_default_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Extra certificate locations to probe on platforms where `openssl-probe`
/// doesn't find the system CA store (e.g., SmartOS/illumos with pkgsrc).
const EXTRA_CERT_FILES: &[&str] = &[
    "/opt/local/etc/openssl/certs/ca-certificates.crt",
    "/etc/ssl/certs/ca-certificates.crt",
];
const EXTRA_CERT_DIRS: &[&str] = &["/opt/local/etc/openssl/certs", "/etc/ssl/certs"];

/// Build a root certificate store with a three-tier fallback:
///
/// 1. Native system certs (via `rustls-native-certs` / `openssl-probe`)
/// 2. Extra platform-specific paths (SmartOS pkgsrc, etc.)
/// 3. Bundled Mozilla roots (via `webpki-roots`) as a last resort
///
/// This handles platforms like SmartOS/illumos where `openssl-probe` doesn't
/// check the paths where certificates are actually installed.
pub async fn build_root_cert_store() -> rustls::RootCertStore {
    let mut root_store = rustls::RootCertStore::empty();

    // 1. Try native certs (respects SSL_CERT_FILE / SSL_CERT_DIR)
    let mut loaded = 0u32;
    let mut skipped = 0u32;
    for cert in rustls_native_certs::load_native_certs().certs {
        if root_store.add(cert).is_ok() {
            loaded += 1;
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        tracing::debug!(
            "Loaded {} native root certs, skipped {} invalid",
            loaded,
            skipped
        );
    }
    if !root_store.is_empty() {
        return root_store;
    }

    // 2. Probe extra platform-specific paths
    load_extra_cert_paths(&mut root_store).await;
    if !root_store.is_empty() {
        return root_store;
    }

    // 3. Fall back to bundled Mozilla roots
    tracing::warn!(
        "no native root certificates found; using bundled Mozilla roots\n\n  \
         If you need to trust additional CAs (e.g., a self-signed certificate),\n  \
         point the TLS library at your certificate store:\n\n    \
         export SSL_CERT_FILE=/path/to/ca-bundle.pem\n    \
         export SSL_CERT_DIR=/path/to/certs/directory"
    );
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    root_store
}

/// Try loading PEM certificates from extra platform-specific paths into the
/// root store. Stops as soon as any certificates are loaded.
async fn load_extra_cert_paths(root_store: &mut rustls::RootCertStore) {
    // Try bundle files first (single file containing many PEM certs)
    for path in EXTRA_CERT_FILES {
        if let Ok(data) = tokio::fs::read(path).await {
            let mut cursor = std::io::Cursor::new(data);
            let mut loaded = 0u32;
            let mut skipped = 0u32;
            for cert in rustls_pemfile::certs(&mut cursor).flatten() {
                if root_store.add(cert).is_ok() {
                    loaded += 1;
                } else {
                    skipped += 1;
                }
            }
            if skipped > 0 {
                tracing::debug!(
                    "Loaded {} root certs, skipped {} invalid from {}",
                    loaded,
                    skipped,
                    path,
                );
            }
            if !root_store.is_empty() {
                return;
            }
        }
    }

    // Try cert directories (individual PEM files, including OpenSSL hash symlinks)
    for dir_path in EXTRA_CERT_DIRS {
        let Ok(mut entries) = tokio::fs::read_dir(dir_path).await else {
            continue;
        };
        let mut loaded = 0u32;
        let mut skipped = 0u32;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Ok(data) = tokio::fs::read(&path).await {
                let mut cursor = std::io::Cursor::new(data);
                for cert in rustls_pemfile::certs(&mut cursor).flatten() {
                    if root_store.add(cert).is_ok() {
                        loaded += 1;
                    } else {
                        skipped += 1;
                    }
                }
            }
        }
        if skipped > 0 {
            tracing::debug!(
                "Loaded {} root certs, skipped {} invalid from {}",
                loaded,
                skipped,
                dir_path,
            );
        }
        if !root_store.is_empty() {
            return;
        }
    }
}

/// Build an HTTP client with proper TLS certificate handling.
///
/// When `insecure` is `true`, TLS certificate validation is skipped entirely.
/// Otherwise, certificates are loaded via [`build_root_cert_store`].
pub async fn build_http_client(insecure: bool) -> Result<reqwest::Client, reqwest::Error> {
    install_default_crypto_provider();

    let mut builder = reqwest::Client::builder().danger_accept_invalid_certs(insecure);

    // Only apply custom root cert store when we actually need to verify
    // certificates. When insecure=true, reqwest's built-in handling of
    // danger_accept_invalid_certs is sufficient — adding a preconfigured
    // TLS config would override it and re-enable chain validation.
    if !insecure {
        let root_store = build_root_cert_store().await;
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        builder = builder.use_preconfigured_tls(tls_config);
    }

    builder.build()
}
