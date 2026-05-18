/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Credentials extracted from a talosconfig context.
pub struct TalosCredentials {
    pub ca_cert_pem: Vec<u8>,
    pub client_cert_pem: Vec<u8>,
    pub client_key_pem: Vec<u8>,
}

/// Top-level talosconfig YAML structure.
#[derive(Deserialize)]
struct TalosConfig {
    context: String,
    contexts: HashMap<String, TalosContext>,
}

/// A single talosconfig context.
#[derive(Deserialize)]
struct TalosContext {
    ca: Option<String>,
    crt: Option<String>,
    key: Option<String>,
}

/// Resolve the path to the talosconfig file.
///
/// Priority: explicit flag > TALOSCONFIG env > ~/.talos/config
pub fn resolve_path(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }

    if let Ok(p) = std::env::var("TALOSCONFIG") {
        return Ok(PathBuf::from(p));
    }

    let home = std::env::var("HOME").context("HOME not set and no talosconfig specified")?;
    Ok(PathBuf::from(home).join(".talos").join("config"))
}

/// Load and decode credentials from a talosconfig file.
pub fn load_credentials(explicit_path: Option<&str>) -> Result<TalosCredentials> {
    let path = resolve_path(explicit_path)?;
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading talosconfig at {}", path.display()))?;

    let cfg: TalosConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("parsing talosconfig at {}", path.display()))?;

    let ctx = cfg
        .contexts
        .get(&cfg.context)
        .with_context(|| format!("context '{}' not found in talosconfig", cfg.context))?;

    let ca_b64 = ctx
        .ca
        .as_deref()
        .context("talosconfig context is missing 'ca' field")?;
    let crt_b64 = ctx
        .crt
        .as_deref()
        .context("talosconfig context is missing 'crt' field")?;
    let key_b64 = ctx
        .key
        .as_deref()
        .context("talosconfig context is missing 'key' field")?;

    let ca_cert_pem = BASE64
        .decode(ca_b64)
        .context("decoding CA certificate base64")?;
    let crt_pem = BASE64
        .decode(crt_b64)
        .context("decoding client certificate base64")?;
    let key_pem = BASE64
        .decode(key_b64)
        .context("decoding client key base64")?;

    // Sanity-check that we got PEM data.
    let ca_str = String::from_utf8_lossy(&ca_cert_pem);
    if !ca_str.contains("BEGIN CERTIFICATE") {
        bail!("CA data does not look like PEM (missing BEGIN CERTIFICATE)");
    }

    /*
     * Talos generates Ed25519 keys with the PEM label "ED25519 PRIVATE KEY"
     * but rustls expects the standard PKCS8 label "PRIVATE KEY". The DER
     * payload is already PKCS8-encoded, so normalise the label.
     */
    let key_pem = normalize_private_key_pem(key_pem);

    Ok(TalosCredentials {
        ca_cert_pem,
        client_cert_pem: crt_pem,
        client_key_pem: key_pem,
    })
}

/// Rewrite non-standard PEM private key labels to the PKCS8 label that
/// rustls expects.
///
/// Talos may emit `ED25519 PRIVATE KEY` (or other algorithm-specific labels)
/// whose DER payload is already PKCS8, so only the label needs changing.
fn normalize_private_key_pem(pem: Vec<u8>) -> Vec<u8> {
    let s = match String::from_utf8(pem) {
        Ok(s) => s,
        Err(e) => return e.into_bytes(),
    };

    if s.contains("BEGIN PRIVATE KEY") && !s.contains("BEGIN ED25519 PRIVATE KEY") {
        return s.into_bytes();
    }

    // Replace algorithm-specific labels with the generic PKCS8 label.
    let out = s
        .replace("BEGIN ED25519 PRIVATE KEY", "BEGIN PRIVATE KEY")
        .replace("END ED25519 PRIVATE KEY", "END PRIVATE KEY");
    out.into_bytes()
}
