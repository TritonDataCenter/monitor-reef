/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

use anyhow::{Context, Result};
use tonic::transport::{Channel, ClientTlsConfig, Uri};

use super::talosconfig;

/// Build a tonic gRPC channel with mTLS to the given Talos endpoint.
///
/// The endpoint should be a hostname or IP address. The channel connects
/// to port 50000 (the Talos gRPC default).
pub async fn connect(endpoint: &str, talosconfig: Option<&str>, verbose: bool) -> Result<Channel> {
    let creds = talosconfig::load_credentials(talosconfig)?;

    let uri: Uri = format!("https://{}:50000", endpoint)
        .parse()
        .context("parsing endpoint URI")?;

    if verbose {
        eprintln!("connecting to {}", uri);
    }

    let tls = ClientTlsConfig::new()
        .ca_certificate(tonic::transport::Certificate::from_pem(&creds.ca_cert_pem))
        .identity(tonic::transport::Identity::from_pem(
            &creds.client_cert_pem,
            &creds.client_key_pem,
        ))
        .domain_name(domain_for_endpoint(endpoint));

    let channel = Channel::builder(uri)
        .tls_config(tls)
        .context("setting TLS config on channel")?
        .connect()
        .await
        .context("connecting to Talos gRPC endpoint")?;

    if verbose {
        eprintln!("connected successfully");
    }

    Ok(channel)
}

/// Derive a TLS domain name for the endpoint.
fn domain_for_endpoint(endpoint: &str) -> String {
    endpoint.to_string()
}
