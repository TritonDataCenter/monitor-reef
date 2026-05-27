// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! On-disk persistence for the agent's console-listener credentials:
//! the per-CN console-ticket key (32 bytes of HS256 secret) and a
//! stable self-signed TLS keypair + leaf cert.
//!
//! These live in a file *next to* the existing `--credential-path`
//! API-key file rather than in it: the API-key file format is unchanged
//! so an in-flight agent that predates this feature keeps working, and
//! the console state has a different lifecycle (the TLS keypair must be
//! stable across boots so tritond's SPKI pin keeps matching; the
//! console-ticket key is delivered exactly once, alongside the API
//! key). The sibling file is `console-credentials.json`, mode 0600.

use std::fs;
use std::io::{ErrorKind, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tritond_auth::CONSOLE_TICKET_KEY_BYTES;

/// File name of the console-credentials sibling file, placed in the
/// same directory as the `--credential-path` API-key file.
const CONSOLE_CREDS_FILE_NAME: &str = "console-credentials.json";

/// PEM bytes match what `RustlsConfig::from_pem` expects and what we
/// persist; the SPKI fingerprint mirrors tritond's `SpkiPinVerifier`.
#[derive(Clone)]
pub struct ConsoleTls {
    /// PEM-encoded self-signed leaf certificate.
    pub cert_pem: Vec<u8>,
    /// PEM-encoded private key for the leaf cert.
    pub key_pem: Vec<u8>,
    /// Lowercase-hex SHA-256 of the leaf cert's `SubjectPublicKeyInfo`
    /// DER. This is exactly what tritond pins (see
    /// `services/tritond/src/console.rs::SpkiPinVerifier`).
    pub spki_sha256_hex: String,
}

/// On-disk JSON shape for `console-credentials.json`. Both the ticket
/// key and the TLS private key are secrets, so the file is written
/// 0600 in a 0700 parent.
#[derive(Debug, Serialize, Deserialize)]
struct ConsoleCredsFile {
    /// Per-CN console-ticket key, lowercase hex (64 chars). Absent on a
    /// CN that registered before this feature shipped — see
    /// [`load_console_ticket_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    console_ticket_key_hex: Option<String>,
    /// Lowercase hex (64 chars). Absent on older registrations; the
    /// migrate listener stays down until the operator re-approves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    migrate_ticket_key_hex: Option<String>,
    /// PEM-encoded self-signed TLS leaf cert.
    tls_cert_pem: String,
    /// PEM-encoded TLS private key.
    tls_key_pem: String,
}

/// Path to the console-credentials file: the `--credential-path` file's
/// directory + `console-credentials.json`.
#[must_use]
pub fn console_creds_path(credential_path: &Path) -> PathBuf {
    let dir = credential_path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(CONSOLE_CREDS_FILE_NAME)
}

/// Read the on-disk file, or `Ok(None)` if it does not exist.
fn read_file(path: &Path) -> Result<Option<ConsoleCredsFile>> {
    match fs::read(path) {
        Ok(bytes) => {
            let parsed: ConsoleCredsFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse console credentials file {}", path.display()))?;
            Ok(Some(parsed))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(e).with_context(|| format!("read console credentials file {}", path.display()))
        }
    }
}

/// Atomically write the on-disk file with mode 0600 in a 0700 parent.
fn write_file(path: &Path, contents: &ConsoleCredsFile) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("console credentials path {} has no parent", path.display()))?;
    if !parent.exists() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "create console credentials parent directory {}",
                parent.display()
            )
        })?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "set 0700 mode on console credentials parent directory {}",
                parent.display()
            )
        })?;
    }

    let json = serde_json::to_vec_pretty(contents).context("serialize console credentials")?;

    let file_name = path
        .file_name()
        .ok_or_else(|| {
            anyhow!(
                "console credentials path {} has no file name",
                path.display()
            )
        })?
        .to_owned();
    let mut tmp_name = file_name.clone();
    tmp_name.push(".tmp");
    let tmp_path = parent.join(&tmp_name);

    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .with_context(|| {
                format!("open temp console credentials file {}", tmp_path.display())
            })?;
        f.write_all(&json)
            .with_context(|| format!("write console credentials to {}", tmp_path.display()))?;
        f.write_all(b"\n")
            .with_context(|| format!("write trailing newline to {}", tmp_path.display()))?;
        f.sync_all().with_context(|| {
            format!("fsync console credentials temp file {}", tmp_path.display())
        })?;
    }

    fs::rename(&tmp_path, path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
        format!(
            "set 0600 mode on console credentials file {}",
            path.display()
        )
    })?;
    Ok(())
}

/// Extract the lowercase-hex SHA-256 of a DER-encoded leaf
/// certificate's `SubjectPublicKeyInfo`, matching tritond's
/// `SpkiPinVerifier` byte-for-byte
/// (`tbs_certificate.subject_pki.raw` → `Sha256` → hex).
fn spki_sha256_hex_of_cert_der(cert_der: &[u8]) -> Result<String> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert_der)
        .map_err(|e| anyhow!("parse self-signed console cert DER: {e}"))?;
    let spki_der = parsed.tbs_certificate.subject_pki.raw;
    let hash: [u8; 32] = Sha256::digest(spki_der).into();
    Ok(hex::encode(hash))
}

/// Pull the first `CERTIFICATE` block out of a PEM document and return
/// its DER bytes — just enough to recompute the SPKI fingerprint
/// without taking a PEM-parsing dependency.
fn first_cert_der_from_pem(pem: &str) -> Result<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let start = pem
        .find(BEGIN)
        .ok_or_else(|| anyhow!("console TLS cert PEM has no CERTIFICATE block"))?
        + BEGIN.len();
    let rest = &pem[start..];
    let end = rest
        .find(END)
        .ok_or_else(|| anyhow!("console TLS cert PEM has an unterminated CERTIFICATE block"))?;
    let b64: String = rest[..end].chars().filter(|c| !c.is_whitespace()).collect();
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .context("base64-decode console TLS cert PEM body")
}

/// Build a [`ConsoleTls`] from PEM strings, computing the SPKI hash.
fn tls_from_pem(cert_pem: &str, key_pem: &str) -> Result<ConsoleTls> {
    let cert_der = first_cert_der_from_pem(cert_pem)?;
    let spki_sha256_hex = spki_sha256_hex_of_cert_der(&cert_der)?;
    Ok(ConsoleTls {
        cert_pem: cert_pem.as_bytes().to_vec(),
        key_pem: key_pem.as_bytes().to_vec(),
        spki_sha256_hex,
    })
}

/// CN is a stable label; admin IP becomes a SAN. tritond pins the
/// SPKI fingerprint, not the name, but a strict client could check it.
fn generate_self_signed(admin_ip: Option<Ipv4Addr>) -> Result<(String, String)> {
    let mut sans: Vec<String> = vec!["tritonagent-console".to_string()];
    if let Some(ip) = admin_ip {
        sans.push(ip.to_string());
    }
    let cert = rcgen::generate_simple_self_signed(sans)
        .map_err(|e| anyhow!("generate self-signed console TLS cert: {e}"))?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.signing_key.serialize_pem();
    Ok((cert_pem, key_pem))
}

/// Stable across boots — regenerating would invalidate the SPKI pin
/// tritond stored at registration. Only generates on first boot or
/// when the file is missing TLS material; ticket keys are preserved.
pub fn load_or_init_tls(credential_path: &Path, admin_ip: Option<Ipv4Addr>) -> Result<ConsoleTls> {
    let path = console_creds_path(credential_path);

    if let Some(existing) = read_file(&path)? {
        if !existing.tls_cert_pem.trim().is_empty() && !existing.tls_key_pem.trim().is_empty() {
            return tls_from_pem(&existing.tls_cert_pem, &existing.tls_key_pem)
                .with_context(|| format!("load console TLS material from {}", path.display()));
        }
        let (cert_pem, key_pem) = generate_self_signed(admin_ip)?;
        let tls = tls_from_pem(&cert_pem, &key_pem)?;
        write_file(
            &path,
            &ConsoleCredsFile {
                console_ticket_key_hex: existing.console_ticket_key_hex,
                migrate_ticket_key_hex: existing.migrate_ticket_key_hex,
                tls_cert_pem: cert_pem,
                tls_key_pem: key_pem,
            },
        )?;
        return Ok(tls);
    }

    // Fresh CN: ticket keys arrive in the registration response.
    let (cert_pem, key_pem) = generate_self_signed(admin_ip)?;
    let tls = tls_from_pem(&cert_pem, &key_pem)?;
    write_file(
        &path,
        &ConsoleCredsFile {
            console_ticket_key_hex: None,
            migrate_ticket_key_hex: None,
            tls_cert_pem: cert_pem,
            tls_key_pem: key_pem,
        },
    )?;
    Ok(tls)
}

/// Writes the key whether or not TLS material exists yet; the next
/// `load_or_init_tls` call will fill in the TLS side.
pub fn save_console_ticket_key(
    credential_path: &Path,
    key_bytes: &[u8; CONSOLE_TICKET_KEY_BYTES],
) -> Result<()> {
    let path = console_creds_path(credential_path);
    let key_hex = hex::encode(key_bytes);
    let (tls_cert_pem, tls_key_pem, migrate_ticket_key_hex) = match read_file(&path)? {
        Some(existing) => (
            existing.tls_cert_pem,
            existing.tls_key_pem,
            existing.migrate_ticket_key_hex,
        ),
        None => (String::new(), String::new(), None),
    };
    write_file(
        &path,
        &ConsoleCredsFile {
            console_ticket_key_hex: Some(key_hex),
            migrate_ticket_key_hex,
            tls_cert_pem,
            tls_key_pem,
        },
    )
}

/// Mirrors `save_console_ticket_key` on the migrate field. Both
/// callers may run in any order; each preserves the other's fields.
pub fn save_migrate_ticket_key(
    credential_path: &Path,
    key_bytes: &[u8; tritond_auth::MIGRATE_TICKET_KEY_BYTES],
) -> Result<()> {
    let path = console_creds_path(credential_path);
    let key_hex = hex::encode(key_bytes);
    let (tls_cert_pem, tls_key_pem, console_ticket_key_hex) = match read_file(&path)? {
        Some(existing) => (
            existing.tls_cert_pem,
            existing.tls_key_pem,
            existing.console_ticket_key_hex,
        ),
        None => (String::new(), String::new(), None),
    };
    write_file(
        &path,
        &ConsoleCredsFile {
            console_ticket_key_hex,
            migrate_ticket_key_hex: Some(key_hex),
            tls_cert_pem,
            tls_key_pem,
        },
    )
}

/// `Ok(None)` whether the file is absent or the field is missing
/// (the migrate listener stays down in that case).
pub fn load_migrate_ticket_key(
    credential_path: &Path,
) -> Result<Option<[u8; tritond_auth::MIGRATE_TICKET_KEY_BYTES]>> {
    let path = console_creds_path(credential_path);
    let Some(file) = read_file(&path)? else {
        return Ok(None);
    };
    let Some(hex_str) = file.migrate_ticket_key_hex else {
        return Ok(None);
    };
    let bytes = hex::decode(hex_str.trim())
        .with_context(|| format!("decode migrate-ticket key hex in {}", path.display()))?;
    if bytes.len() != tritond_auth::MIGRATE_TICKET_KEY_BYTES {
        bail!(
            "migrate-ticket key in {} is {} bytes, expected {}",
            path.display(),
            bytes.len(),
            tritond_auth::MIGRATE_TICKET_KEY_BYTES,
        );
    }
    let mut out = [0u8; tritond_auth::MIGRATE_TICKET_KEY_BYTES];
    out.copy_from_slice(&bytes);
    Ok(Some(out))
}

/// `Ok(None)` whether the file is absent or the field is missing.
pub fn load_console_ticket_key(
    credential_path: &Path,
) -> Result<Option<[u8; CONSOLE_TICKET_KEY_BYTES]>> {
    let path = console_creds_path(credential_path);
    let Some(file) = read_file(&path)? else {
        return Ok(None);
    };
    let Some(hex_str) = file.console_ticket_key_hex else {
        return Ok(None);
    };
    let bytes = hex::decode(hex_str.trim())
        .with_context(|| format!("decode console-ticket key hex in {}", path.display()))?;
    if bytes.len() != CONSOLE_TICKET_KEY_BYTES {
        bail!(
            "console-ticket key in {} is {} bytes, expected {}",
            path.display(),
            bytes.len(),
            CONSOLE_TICKET_KEY_BYTES,
        );
    }
    let mut out = [0u8; CONSOLE_TICKET_KEY_BYTES];
    out.copy_from_slice(&bytes);
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn console_creds_path_is_sibling_of_credential_path() {
        let p = console_creds_path(Path::new("/var/lib/tritonagent/credentials"));
        assert_eq!(
            p,
            PathBuf::from("/var/lib/tritonagent/console-credentials.json")
        );
    }

    #[test]
    fn load_or_init_tls_generates_then_reloads_stable_keypair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cred = dir.path().join("nested").join("credentials");

        let first = load_or_init_tls(&cred, Some(Ipv4Addr::new(10, 0, 0, 5))).expect("first");
        assert_eq!(first.spki_sha256_hex.len(), 64);
        assert!(first.spki_sha256_hex.bytes().all(|b| b.is_ascii_hexdigit()));

        // File is 0600.
        let creds_file = console_creds_path(&cred);
        let meta = std::fs::metadata(&creds_file).expect("meta");
        assert_eq!(meta.mode() & 0o777, 0o600);

        // Second call loads the same keypair → same SPKI fingerprint.
        let second = load_or_init_tls(&cred, Some(Ipv4Addr::new(10, 0, 0, 5))).expect("second");
        assert_eq!(first.spki_sha256_hex, second.spki_sha256_hex);
        assert_eq!(first.cert_pem, second.cert_pem);
    }

    #[test]
    fn console_ticket_key_round_trips_without_clobbering_tls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cred = dir.path().join("credentials");

        let tls = load_or_init_tls(&cred, None).expect("init tls");
        assert!(load_console_ticket_key(&cred).expect("load").is_none());

        let key = [0x5au8; CONSOLE_TICKET_KEY_BYTES];
        save_console_ticket_key(&cred, &key).expect("save key");
        assert_eq!(load_console_ticket_key(&cred).expect("load"), Some(key));

        // TLS material survived the key write.
        let tls2 = load_or_init_tls(&cred, None).expect("reload tls");
        assert_eq!(tls.cert_pem, tls2.cert_pem);
        assert_eq!(tls.spki_sha256_hex, tls2.spki_sha256_hex);
    }

    #[test]
    fn save_key_before_tls_then_init_keeps_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cred = dir.path().join("credentials");

        let key = [0x11u8; CONSOLE_TICKET_KEY_BYTES];
        save_console_ticket_key(&cred, &key).expect("save key");
        let _tls = load_or_init_tls(&cred, None).expect("init tls");
        assert_eq!(load_console_ticket_key(&cred).expect("load"), Some(key));
    }

    #[test]
    fn spki_hex_matches_x509_parser_subject_pki() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cred = dir.path().join("credentials");
        let tls = load_or_init_tls(&cred, None).expect("init tls");

        let cert_der =
            first_cert_der_from_pem(std::str::from_utf8(&tls.cert_pem).unwrap()).unwrap();
        let (_, parsed) = x509_parser::parse_x509_certificate(&cert_der).expect("parse");
        let spki = parsed.tbs_certificate.subject_pki.raw;
        let hash: [u8; 32] = Sha256::digest(spki).into();
        assert_eq!(tls.spki_sha256_hex, hex::encode(hash));
    }
}
