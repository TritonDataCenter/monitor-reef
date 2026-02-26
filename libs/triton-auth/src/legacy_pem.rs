// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Support for traditional PEM key formats (PKCS#1, SEC1, DSA)
//!
//! The `ssh-key` crate only supports OpenSSH format keys, but many existing
//! keys are in traditional PEM formats:
//!
//! - RSA: PKCS#1 (`-----BEGIN RSA PRIVATE KEY-----`)
//! - ECDSA: SEC1 (`-----BEGIN EC PRIVATE KEY-----`)
//! - DSA: OpenSSL DSA (`-----BEGIN DSA PRIVATE KEY-----`)
//!
//! This module provides parsing and signing support for these formats.

use crate::error::AuthError;
use crate::signature::KeyType;
use sha1::Digest as Sha1Digest;
use signature::SignatureEncoding;

/// Detected PEM key format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PemKeyFormat {
    /// OpenSSH format (ssh-key crate handles this)
    OpenSsh,
    /// PKCS#1 RSA private key
    Pkcs1Rsa,
    /// SEC1 EC private key
    Sec1Ecdsa,
    /// OpenSSL DSA private key
    Dsa,
    /// PKCS#8 private key (any algorithm)
    Pkcs8,
    /// Encrypted PKCS#1 (Proc-Type header present)
    EncryptedPkcs1,
    /// Unknown format
    Unknown,
}

impl PemKeyFormat {
    /// Detect the key format from PEM data
    pub fn detect(pem_data: &str) -> Self {
        // Check for encrypted key first (has Proc-Type header)
        if pem_data.contains("Proc-Type:") && pem_data.contains("ENCRYPTED") {
            return Self::EncryptedPkcs1;
        }

        if pem_data.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
            Self::OpenSsh
        } else if pem_data.contains("-----BEGIN RSA PRIVATE KEY-----") {
            Self::Pkcs1Rsa
        } else if pem_data.contains("-----BEGIN EC PRIVATE KEY-----") {
            Self::Sec1Ecdsa
        } else if pem_data.contains("-----BEGIN DSA PRIVATE KEY-----") {
            Self::Dsa
        } else if pem_data.contains("-----BEGIN PRIVATE KEY-----") {
            Self::Pkcs8
        } else {
            Self::Unknown
        }
    }
}

/// A private key loaded from traditional PEM format
///
/// This enum holds keys parsed from various PEM formats that can be used
/// for signing operations.
pub enum LegacyPrivateKey {
    /// RSA key from PKCS#1 format
    Rsa(rsa::RsaPrivateKey),
    /// ECDSA P-256 key from SEC1 format
    EcdsaP256(p256::ecdsa::SigningKey),
    /// ECDSA P-384 key from SEC1 format
    EcdsaP384(p384::ecdsa::SigningKey),
    /// DSA key
    Dsa(dsa::SigningKey),
    /// OpenSSH format key (delegated to ssh-key crate)
    OpenSsh(ssh_key::PrivateKey),
}

impl LegacyPrivateKey {
    /// Load a private key from PEM data, detecting format automatically
    pub fn from_pem(pem_data: &str, passphrase: Option<&str>) -> Result<Self, AuthError> {
        let format = PemKeyFormat::detect(pem_data);

        match format {
            PemKeyFormat::OpenSsh => {
                let key = ssh_key::PrivateKey::from_openssh(pem_data.as_bytes())
                    .map_err(|e| AuthError::KeyLoadError(format!("OpenSSH parse error: {}", e)))?;

                if key.is_encrypted() {
                    if let Some(pass) = passphrase {
                        let decrypted = key.decrypt(pass.as_bytes()).map_err(|e| {
                            AuthError::KeyLoadError(format!("Failed to decrypt key: {}", e))
                        })?;
                        Ok(Self::OpenSsh(decrypted))
                    } else {
                        Err(AuthError::KeyLoadError(
                            "Key is encrypted but no passphrase provided".into(),
                        ))
                    }
                } else {
                    Ok(Self::OpenSsh(key))
                }
            }
            PemKeyFormat::Pkcs1Rsa => Self::load_pkcs1_rsa(pem_data),
            PemKeyFormat::Sec1Ecdsa => Self::load_sec1_ecdsa(pem_data),
            PemKeyFormat::Dsa => Self::load_dsa(pem_data),
            PemKeyFormat::Pkcs8 => Self::load_pkcs8(pem_data),
            PemKeyFormat::EncryptedPkcs1 => {
                if let Some(pass) = passphrase {
                    Self::load_encrypted_pkcs1(pem_data, pass)
                } else {
                    Err(AuthError::KeyEncrypted(
                        "Key is encrypted but no passphrase provided".into(),
                    ))
                }
            }
            PemKeyFormat::Unknown => Err(AuthError::KeyLoadError(
                "Unknown key format. Supported formats: OpenSSH, PKCS#1 RSA, SEC1 ECDSA, DSA"
                    .into(),
            )),
        }
    }

    /// Load PKCS#1 RSA private key
    fn load_pkcs1_rsa(pem_data: &str) -> Result<Self, AuthError> {
        use rsa::pkcs1::DecodeRsaPrivateKey;
        let key = rsa::RsaPrivateKey::from_pkcs1_pem(pem_data)
            .map_err(|e| AuthError::KeyLoadError(format!("PKCS#1 RSA parse error: {}", e)))?;
        Ok(Self::Rsa(key))
    }

    /// Load SEC1 ECDSA private key
    fn load_sec1_ecdsa(pem_data: &str) -> Result<Self, AuthError> {
        // Try P-256 first, then P-384
        // SEC1 format includes OID that identifies the curve

        // Try P-256
        if let Ok(key) = p256::SecretKey::from_sec1_pem(pem_data) {
            return Ok(Self::EcdsaP256(p256::ecdsa::SigningKey::from(key)));
        }

        // Try P-384
        if let Ok(key) = p384::SecretKey::from_sec1_pem(pem_data) {
            return Ok(Self::EcdsaP384(p384::ecdsa::SigningKey::from(key)));
        }

        Err(AuthError::KeyLoadError(
            "Failed to parse SEC1 ECDSA key. Supported curves: P-256, P-384".into(),
        ))
    }

    /// Load DSA private key
    fn load_dsa(pem_data: &str) -> Result<Self, AuthError> {
        // DSA keys in traditional PEM format need manual parsing
        // The dsa crate expects the key components directly

        // Parse the PEM to get DER bytes
        let pem = pem_rfc7468::decode_vec(pem_data.as_bytes())
            .map_err(|e| AuthError::KeyLoadError(format!("DSA PEM decode error: {}", e)))?;

        // DSA private key ASN.1 structure:
        // DSAPrivateKey ::= SEQUENCE {
        //   version INTEGER,
        //   p INTEGER,
        //   q INTEGER,
        //   g INTEGER,
        //   y INTEGER,  (public key)
        //   x INTEGER   (private key)
        // }

        use dsa::Components;

        // Parse the ASN.1 sequence manually
        let der = pem.1;
        let (p, q, g, y, x) = parse_dsa_der(&der)?;

        let components = Components::from_components(p, q, g)
            .map_err(|e| AuthError::KeyLoadError(format!("Invalid DSA components: {}", e)))?;

        // First create a VerifyingKey from components and y
        let verifying_key = dsa::VerifyingKey::from_components(components, y)
            .map_err(|e| AuthError::KeyLoadError(format!("Invalid DSA public key: {}", e)))?;

        // Then create SigningKey from verifying key and private component x
        let signing_key = dsa::SigningKey::from_components(verifying_key, x)
            .map_err(|e| AuthError::KeyLoadError(format!("Invalid DSA key: {}", e)))?;

        Ok(Self::Dsa(signing_key))
    }

    /// Load PKCS#8 private key
    fn load_pkcs8(pem_data: &str) -> Result<Self, AuthError> {
        use pkcs8::DecodePrivateKey;

        // Try RSA first
        if let Ok(key) = rsa::RsaPrivateKey::from_pkcs8_pem(pem_data) {
            return Ok(Self::Rsa(key));
        }

        // Try P-256
        if let Ok(key) = p256::SecretKey::from_pkcs8_pem(pem_data) {
            return Ok(Self::EcdsaP256(p256::ecdsa::SigningKey::from(key)));
        }

        // Try P-384
        if let Ok(key) = p384::SecretKey::from_pkcs8_pem(pem_data) {
            return Ok(Self::EcdsaP384(p384::ecdsa::SigningKey::from(key)));
        }

        Err(AuthError::KeyLoadError(
            "Failed to parse PKCS#8 key. Supported algorithms: RSA, ECDSA P-256/P-384".into(),
        ))
    }

    /// Load an encrypted PKCS#1 PEM key with passphrase
    ///
    /// Supports the traditional OpenSSL encrypted PEM format with:
    /// - AES-128-CBC
    /// - AES-256-CBC
    /// - DES-EDE3-CBC (3DES)
    ///
    /// Key derivation uses the OpenSSL `EVP_BytesToKey` scheme (MD5-based KDF).
    fn load_encrypted_pkcs1(pem_data: &str, passphrase: &str) -> Result<Self, AuthError> {
        // Parse the PEM structure manually to extract DEK-Info and body
        let (cipher_name, iv_hex, body_b64) = parse_encrypted_pem(pem_data)?;

        // Decode IV from hex
        let iv = hex_decode(&iv_hex)
            .map_err(|e| AuthError::KeyLoadError(format!("Invalid IV hex in DEK-Info: {}", e)))?;

        // Decode the base64 body
        let body = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &body_b64)
            .map_err(|e| {
                AuthError::KeyLoadError(format!("Invalid base64 in encrypted PEM: {}", e))
            })?;

        // Decrypt using EVP_BytesToKey KDF + the appropriate cipher
        let decrypted = decrypt_pem_body(&cipher_name, passphrase.as_bytes(), &iv, &body)?;

        // Parse decrypted DER as PKCS#1 RSA
        use rsa::pkcs1::DecodeRsaPrivateKey;
        let key = rsa::RsaPrivateKey::from_pkcs1_der(&decrypted).map_err(|e| {
            AuthError::KeyLoadError(format!(
                "Failed to parse decrypted PKCS#1 RSA key (wrong passphrase?): {}",
                e,
            ))
        })?;

        Ok(Self::Rsa(key))
    }

    /// Get the key type for HTTP Signature algorithm selection
    pub fn key_type(&self) -> Result<KeyType, AuthError> {
        match self {
            Self::Rsa(_) => Ok(KeyType::Rsa),
            Self::EcdsaP256(_) => Ok(KeyType::Ecdsa256),
            Self::EcdsaP384(_) => Ok(KeyType::Ecdsa384),
            Self::Dsa(_) => Ok(KeyType::Dsa),
            Self::OpenSsh(key) => KeyType::from_private_key(key),
        }
    }

    /// Get the public key in SSH wire format for fingerprinting
    pub fn public_key_blob(&self) -> Result<Vec<u8>, AuthError> {
        match self {
            Self::OpenSsh(key) => {
                // ssh-key provides this directly
                Ok(key.public_key().to_bytes().map_err(|e| {
                    AuthError::KeyLoadError(format!("Failed to encode public key: {}", e))
                })?)
            }
            Self::Rsa(key) => {
                // SSH RSA public key format:
                // string "ssh-rsa"
                // mpint e (public exponent)
                // mpint n (modulus)
                use rsa::traits::PublicKeyParts;
                let e = key.e().to_bytes_be();
                let n = key.n().to_bytes_be();

                let mut blob = Vec::new();
                // Write key type string
                write_ssh_string(&mut blob, b"ssh-rsa");
                // Write e as mpint
                write_ssh_mpint(&mut blob, &e);
                // Write n as mpint
                write_ssh_mpint(&mut blob, &n);

                Ok(blob)
            }
            Self::EcdsaP256(key) => {
                // SSH ECDSA public key format:
                // string "ecdsa-sha2-nistp256"
                // string "nistp256"
                // string Q (public point in uncompressed form)
                use p256::ecdsa::VerifyingKey;
                let verifying_key = VerifyingKey::from(key);
                let point = verifying_key.to_encoded_point(false);

                let mut blob = Vec::new();
                write_ssh_string(&mut blob, b"ecdsa-sha2-nistp256");
                write_ssh_string(&mut blob, b"nistp256");
                write_ssh_string(&mut blob, point.as_bytes());

                Ok(blob)
            }
            Self::EcdsaP384(key) => {
                use p384::ecdsa::VerifyingKey;
                let verifying_key = VerifyingKey::from(key);
                let point = verifying_key.to_encoded_point(false);

                let mut blob = Vec::new();
                write_ssh_string(&mut blob, b"ecdsa-sha2-nistp384");
                write_ssh_string(&mut blob, b"nistp384");
                write_ssh_string(&mut blob, point.as_bytes());

                Ok(blob)
            }
            Self::Dsa(key) => {
                // SSH DSA public key format:
                // string "ssh-dss"
                // mpint p
                // mpint q
                // mpint g
                // mpint y (public key)
                let verifying_key = key.verifying_key();
                let components = verifying_key.components();

                let mut blob = Vec::new();
                write_ssh_string(&mut blob, b"ssh-dss");
                write_ssh_mpint(&mut blob, &components.p().to_bytes_be());
                write_ssh_mpint(&mut blob, &components.q().to_bytes_be());
                write_ssh_mpint(&mut blob, &components.g().to_bytes_be());
                write_ssh_mpint(&mut blob, &verifying_key.y().to_bytes_be());

                Ok(blob)
            }
        }
    }

    /// Sign data with this key
    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>, AuthError> {
        match self {
            Self::OpenSsh(key) => {
                // Use ssh-key's signing
                let key_type = KeyType::from_private_key(key)?;
                let hash_alg = match key_type {
                    KeyType::Rsa | KeyType::Dsa | KeyType::Ecdsa256 | KeyType::Ecdsa384 => {
                        ssh_key::HashAlg::Sha256
                    }
                    KeyType::Ecdsa521 | KeyType::Ed25519 => ssh_key::HashAlg::Sha512,
                };

                let sig = key
                    .sign("", hash_alg, data)
                    .map_err(|e| AuthError::SigningError(format!("SSH signing failed: {}", e)))?;

                Ok(sig.signature_bytes().to_vec())
            }
            Self::Rsa(key) => {
                // RSA-SHA256 signature using PKCS#1 v1.5
                use rsa::pkcs1v15::SigningKey;
                use rsa::signature::Signer;
                use sha2::Sha256;

                let signing_key = SigningKey::<Sha256>::new(key.clone());
                let signature = signing_key
                    .try_sign(data)
                    .map_err(|e| AuthError::SigningError(format!("RSA signing failed: {}", e)))?;

                Ok(signature.to_vec())
            }
            Self::EcdsaP256(key) => {
                // ECDSA-SHA256 signature
                use p256::ecdsa::signature::Signer;

                let signature: p256::ecdsa::Signature = key
                    .try_sign(data)
                    .map_err(|e| AuthError::SigningError(format!("ECDSA signing failed: {}", e)))?;

                // Return raw r||s format (not DER)
                Ok(signature.to_bytes().to_vec())
            }
            Self::EcdsaP384(key) => {
                use p384::ecdsa::signature::Signer;

                let signature: p384::ecdsa::Signature = key
                    .try_sign(data)
                    .map_err(|e| AuthError::SigningError(format!("ECDSA signing failed: {}", e)))?;

                Ok(signature.to_bytes().to_vec())
            }
            Self::Dsa(key) => {
                // DSA-SHA1 signature (DSA traditionally uses SHA-1)
                use dsa::signature::DigestSigner;
                use sha1::Sha1;

                let mut digest = Sha1::new();
                Sha1Digest::update(&mut digest, data);

                let signature: dsa::Signature = key
                    .try_sign_digest(digest)
                    .map_err(|e| AuthError::SigningError(format!("DSA signing failed: {}", e)))?;

                // DSA signature needs to be in SSH format: two 20-byte integers
                // The dsa crate provides r() and s() accessors
                let r_bytes = signature.r().to_bytes_be();
                let s_bytes = signature.s().to_bytes_be();

                if r_bytes.len() > 20 || s_bytes.len() > 20 {
                    return Err(AuthError::SigningError(format!(
                        "DSA signature component exceeds 20 bytes (r={}, s={})",
                        r_bytes.len(),
                        s_bytes.len()
                    )));
                }

                // Pad to 20 bytes each (SHA-1 output size)
                let mut sig_bytes = vec![0u8; 40];
                let r_start = 20 - r_bytes.len();
                let s_start = 40 - s_bytes.len();
                sig_bytes[r_start..20].copy_from_slice(&r_bytes);
                sig_bytes[s_start..40].copy_from_slice(&s_bytes);

                Ok(sig_bytes)
            }
        }
    }
}

/// Parse DSA private key from DER encoding
fn parse_dsa_der(
    der: &[u8],
) -> Result<
    (
        dsa::BigUint,
        dsa::BigUint,
        dsa::BigUint,
        dsa::BigUint,
        dsa::BigUint,
    ),
    AuthError,
> {
    // Simple ASN.1 DER parser for DSA private key
    // DSAPrivateKey ::= SEQUENCE {
    //   version INTEGER,
    //   p INTEGER,
    //   q INTEGER,
    //   g INTEGER,
    //   y INTEGER,
    //   x INTEGER
    // }

    let mut pos = 0;

    // Check SEQUENCE tag
    if der.get(pos) != Some(&0x30) {
        return Err(AuthError::KeyLoadError(
            "Invalid DSA key: not a SEQUENCE".into(),
        ));
    }
    pos += 1;

    // Skip length (may be multi-byte)
    pos = skip_asn1_length(der, pos)?;

    // Skip version INTEGER
    pos = skip_asn1_integer(der, pos)?;

    // Read p
    let (p, new_pos) = read_asn1_integer(der, pos)?;
    pos = new_pos;

    // Read q
    let (q, new_pos) = read_asn1_integer(der, pos)?;
    pos = new_pos;

    // Read g
    let (g, new_pos) = read_asn1_integer(der, pos)?;
    pos = new_pos;

    // Read y (public key)
    let (y, new_pos) = read_asn1_integer(der, pos)?;
    pos = new_pos;

    // Read x (private key)
    let (x, _) = read_asn1_integer(der, pos)?;

    Ok((
        dsa::BigUint::from_bytes_be(&p),
        dsa::BigUint::from_bytes_be(&q),
        dsa::BigUint::from_bytes_be(&g),
        dsa::BigUint::from_bytes_be(&y),
        dsa::BigUint::from_bytes_be(&x),
    ))
}

fn skip_asn1_length(der: &[u8], pos: usize) -> Result<usize, AuthError> {
    let len_byte = *der
        .get(pos)
        .ok_or_else(|| AuthError::KeyLoadError("Unexpected end of DER data".into()))?;

    if len_byte & 0x80 == 0 {
        // Short form
        Ok(pos + 1)
    } else {
        // Long form
        let num_octets = (len_byte & 0x7f) as usize;
        Ok(pos + 1 + num_octets)
    }
}

fn skip_asn1_integer(der: &[u8], pos: usize) -> Result<usize, AuthError> {
    // Check INTEGER tag
    if der.get(pos) != Some(&0x02) {
        return Err(AuthError::KeyLoadError("Expected INTEGER tag".into()));
    }

    let (_, end_pos) = read_asn1_integer(der, pos)?;
    Ok(end_pos)
}

fn read_asn1_integer(der: &[u8], pos: usize) -> Result<(Vec<u8>, usize), AuthError> {
    // Check INTEGER tag
    if der.get(pos) != Some(&0x02) {
        return Err(AuthError::KeyLoadError("Expected INTEGER tag".into()));
    }

    let len_pos = pos + 1;
    let len_byte = *der
        .get(len_pos)
        .ok_or_else(|| AuthError::KeyLoadError("Unexpected end of DER data".into()))?;

    let (length, data_pos) = if len_byte & 0x80 == 0 {
        // Short form
        (len_byte as usize, len_pos + 1)
    } else {
        // Long form
        let num_octets = (len_byte & 0x7f) as usize;
        let mut length: usize = 0;
        for i in 0..num_octets {
            let byte = *der
                .get(len_pos + 1 + i)
                .ok_or_else(|| AuthError::KeyLoadError("Unexpected end of DER data".into()))?;
            length = (length << 8) | (byte as usize);
        }
        (length, len_pos + 1 + num_octets)
    };

    let data = der
        .get(data_pos..data_pos + length)
        .ok_or_else(|| AuthError::KeyLoadError("Unexpected end of DER data".into()))?;

    // Strip leading zero if present (ASN.1 uses it for positive numbers with high bit set)
    let data = if !data.is_empty() && data[0] == 0 {
        &data[1..]
    } else {
        data
    };

    Ok((data.to_vec(), data_pos + length))
}

/// Write an SSH string (4-byte length prefix + data)
fn write_ssh_string(buf: &mut Vec<u8>, data: &[u8]) {
    buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buf.extend_from_slice(data);
}

/// Write an SSH mpint (4-byte length + data, with leading zero for negative prevention)
fn write_ssh_mpint(buf: &mut Vec<u8>, data: &[u8]) {
    if data.is_empty() {
        buf.extend_from_slice(&0u32.to_be_bytes());
        return;
    }

    // Skip leading zeros (except one if needed for sign bit)
    let mut start = 0;
    while start < data.len() - 1 && data[start] == 0 {
        start += 1;
    }
    let data = &data[start..];

    // Add leading zero if high bit is set (to indicate positive number)
    if !data.is_empty() && data[0] & 0x80 != 0 {
        buf.extend_from_slice(&((data.len() + 1) as u32).to_be_bytes());
        buf.push(0);
        buf.extend_from_slice(data);
    } else {
        buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
        buf.extend_from_slice(data);
    }
}

/// Parse encrypted PEM to extract cipher name, IV hex, and base64 body
fn parse_encrypted_pem(pem_data: &str) -> Result<(String, String, String), AuthError> {
    let mut in_headers = false;
    let mut cipher_name = String::new();
    let mut iv_hex = String::new();
    let mut body_lines = Vec::new();
    let mut past_headers = false;

    for line in pem_data.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN") {
            in_headers = true;
            continue;
        }
        if line.starts_with("-----END") {
            break;
        }
        if in_headers && !past_headers {
            if line.is_empty() {
                // Blank line separates headers from body
                past_headers = true;
                continue;
            }
            if let Some(dek) = line.strip_prefix("DEK-Info:") {
                let dek = dek.trim();
                if let Some((cipher, iv)) = dek.split_once(',') {
                    cipher_name = cipher.trim().to_string();
                    iv_hex = iv.trim().to_string();
                }
            }
            continue;
        }
        if !line.is_empty() {
            body_lines.push(line);
        }
    }

    if cipher_name.is_empty() || iv_hex.is_empty() {
        return Err(AuthError::KeyLoadError(
            "Missing DEK-Info header in encrypted PEM".into(),
        ));
    }

    Ok((cipher_name, iv_hex, body_lines.join("")))
}

/// Decode hex string to bytes
fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("Odd-length hex string".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Decrypt PEM body using OpenSSL's EVP_BytesToKey KDF + specified cipher
fn decrypt_pem_body(
    cipher_name: &str,
    passphrase: &[u8],
    iv: &[u8],
    body: &[u8],
) -> Result<Vec<u8>, AuthError> {
    use cbc::cipher::{BlockDecryptMut, KeyIvInit};

    match cipher_name {
        "AES-128-CBC" => {
            let key = evp_bytes_to_key::<16>(passphrase, &iv[..8]);
            type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
            let mut buf = body.to_vec();
            let decrypted = Aes128CbcDec::new_from_slices(&key, iv)
                .map_err(|e| AuthError::KeyLoadError(format!("Cipher init error: {}", e)))?
                .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf)
                .map_err(|e| AuthError::KeyLoadError(format!("Decryption failed: {}", e)))?;
            Ok(decrypted.to_vec())
        }
        "AES-256-CBC" => {
            let key = evp_bytes_to_key::<32>(passphrase, &iv[..8]);
            type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
            let mut buf = body.to_vec();
            let decrypted = Aes256CbcDec::new_from_slices(&key, iv)
                .map_err(|e| AuthError::KeyLoadError(format!("Cipher init error: {}", e)))?
                .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf)
                .map_err(|e| AuthError::KeyLoadError(format!("Decryption failed: {}", e)))?;
            Ok(decrypted.to_vec())
        }
        "DES-EDE3-CBC" => {
            let key = evp_bytes_to_key::<24>(passphrase, &iv[..8]);
            type Des3CbcDec = cbc::Decryptor<des::TdesEde3>;
            let mut buf = body.to_vec();
            let decrypted = Des3CbcDec::new_from_slices(&key, iv)
                .map_err(|e| AuthError::KeyLoadError(format!("Cipher init error: {}", e)))?
                .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf)
                .map_err(|e| AuthError::KeyLoadError(format!("Decryption failed: {}", e)))?;
            Ok(decrypted.to_vec())
        }
        _ => Err(AuthError::KeyLoadError(format!(
            "Unsupported cipher: {}. Supported: AES-128-CBC, AES-256-CBC, DES-EDE3-CBC",
            cipher_name
        ))),
    }
}

/// OpenSSL EVP_BytesToKey KDF (MD5-based)
///
/// Derives a key of `KEY_LEN` bytes from passphrase + salt using iterated MD5.
/// This matches the key derivation used by `openssl enc` and traditional PEM encryption.
fn evp_bytes_to_key<const KEY_LEN: usize>(passphrase: &[u8], salt: &[u8]) -> [u8; KEY_LEN] {
    use md5::{Digest, Md5};

    let mut key = [0u8; KEY_LEN];
    let mut d = Vec::new();
    let mut offset = 0;

    while offset < KEY_LEN {
        let mut hasher = Md5::new();
        if !d.is_empty() {
            hasher.update(&d);
        }
        hasher.update(passphrase);
        hasher.update(salt);
        d = hasher.finalize().to_vec();

        let copy_len = std::cmp::min(d.len(), KEY_LEN - offset);
        key[offset..offset + copy_len].copy_from_slice(&d[..copy_len]);
        offset += copy_len;
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_detection() {
        assert_eq!(
            PemKeyFormat::detect("-----BEGIN OPENSSH PRIVATE KEY-----"),
            PemKeyFormat::OpenSsh
        );
        assert_eq!(
            PemKeyFormat::detect("-----BEGIN RSA PRIVATE KEY-----"),
            PemKeyFormat::Pkcs1Rsa
        );
        assert_eq!(
            PemKeyFormat::detect("-----BEGIN EC PRIVATE KEY-----"),
            PemKeyFormat::Sec1Ecdsa
        );
        assert_eq!(
            PemKeyFormat::detect("-----BEGIN DSA PRIVATE KEY-----"),
            PemKeyFormat::Dsa
        );
        assert_eq!(
            PemKeyFormat::detect("-----BEGIN PRIVATE KEY-----"),
            PemKeyFormat::Pkcs8
        );
        assert_eq!(
            PemKeyFormat::detect(
                "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES"
            ),
            PemKeyFormat::EncryptedPkcs1
        );
    }

    #[test]
    fn test_write_ssh_mpint_empty_input() {
        let mut buf = Vec::new();
        write_ssh_mpint(&mut buf, &[]);
        // Empty mpint: 4-byte zero length
        assert_eq!(buf, vec![0, 0, 0, 0]);
    }

    #[test]
    fn test_write_ssh_mpint_with_high_bit() {
        let mut buf = Vec::new();
        write_ssh_mpint(&mut buf, &[0x80]);
        // Should prepend a zero byte to avoid negative interpretation
        assert_eq!(buf, vec![0, 0, 0, 2, 0, 0x80]);
    }

    #[test]
    fn test_write_ssh_mpint_normal() {
        let mut buf = Vec::new();
        write_ssh_mpint(&mut buf, &[0x01, 0x02]);
        assert_eq!(buf, vec![0, 0, 0, 2, 0x01, 0x02]);
    }
}
