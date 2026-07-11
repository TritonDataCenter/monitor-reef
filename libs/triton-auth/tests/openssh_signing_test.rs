// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! OpenSSH-format signing regression tests for triton-auth
//!
//! These guard against the "namespace invalid" bug: signing a file-based
//! OpenSSH key previously went through `ssh_key::PrivateKey::sign("", ..)`,
//! which produces an SSHSIG blob and rejects the empty namespace. That is the
//! whole reason ssh-agent "worked" but `~/.ssh` keys did not. Every case below
//! both signs AND verifies via the HTTP-Sig verifier to ensure the wire format
//! matches what CloudAPI expects.

use rand_core::OsRng;
use ssh_key::private::{EcdsaKeypair, Ed25519Keypair, RsaKeypair};
use ssh_key::{EcdsaCurve, LineEnding, PrivateKey};
use triton_auth::http_sig::verify_signature;
use triton_auth::legacy_pem::{LegacyPrivateKey, PemKeyFormat};

const SIGNING_STRING: &[u8] =
    b"(request-target): get /foo/machines\ndate: Mon, 15 Dec 2025 10:30:00 GMT";

#[test]
fn test_openssh_ed25519_sign_verifies() {
    let ssh_priv = PrivateKey::from(Ed25519Keypair::random(&mut OsRng));
    let public_key = ssh_priv.public_key().clone();

    let key = LegacyPrivateKey::OpenSsh(ssh_priv);
    let sig = key
        .sign(SIGNING_STRING)
        .expect("OpenSSH ed25519 signing must not fail with 'namespace invalid'");

    assert_eq!(sig.len(), 64, "ed25519 signature must be raw 64 bytes");
    verify_signature(&public_key, "ed25519", SIGNING_STRING, &sig)
        .expect("ed25519 signature must verify");
}

#[test]
fn test_openssh_ecdsa_p256_sign_verifies() {
    let ssh_priv = PrivateKey::from(
        EcdsaKeypair::random(&mut OsRng, EcdsaCurve::NistP256).expect("p256 keygen"),
    );
    let public_key = ssh_priv.public_key().clone();

    let key = LegacyPrivateKey::OpenSsh(ssh_priv);
    let sig = key.sign(SIGNING_STRING).expect("OpenSSH p256 signing");

    assert_eq!(sig[0], 0x30, "ECDSA signature must be DER-encoded");
    verify_signature(&public_key, "ecdsa-sha256", SIGNING_STRING, &sig)
        .expect("p256 signature must verify");
}

#[test]
fn test_openssh_ecdsa_p384_sign_verifies() {
    let ssh_priv = PrivateKey::from(
        EcdsaKeypair::random(&mut OsRng, EcdsaCurve::NistP384).expect("p384 keygen"),
    );
    let public_key = ssh_priv.public_key().clone();

    let key = LegacyPrivateKey::OpenSsh(ssh_priv);
    let sig = key.sign(SIGNING_STRING).expect("OpenSSH p384 signing");

    assert_eq!(sig[0], 0x30, "ECDSA signature must be DER-encoded");
    verify_signature(&public_key, "ecdsa-sha384", SIGNING_STRING, &sig)
        .expect("p384 signature must verify");
}

#[test]
fn test_openssh_rsa_sign_verifies() {
    let ssh_priv = PrivateKey::from(RsaKeypair::random(&mut OsRng, 2048).expect("rsa keygen"));
    let public_key = ssh_priv.public_key().clone();

    let key = LegacyPrivateKey::OpenSsh(ssh_priv);
    let sig = key.sign(SIGNING_STRING).expect("OpenSSH rsa signing");

    assert_eq!(sig.len(), 256, "2048-bit RSA signature must be 256 bytes");
    verify_signature(&public_key, "rsa-sha256", SIGNING_STRING, &sig)
        .expect("rsa signature must verify");
}

/// End-to-end regression for the reported failure: a key read from disk in
/// OpenSSH PEM format must sign successfully (no "namespace invalid") and
/// produce a verifiable signature.
#[test]
fn test_openssh_pem_roundtrip_sign_verifies() {
    let ssh_priv = PrivateKey::from(Ed25519Keypair::random(&mut OsRng));
    let public_key = ssh_priv.public_key().clone();

    // Serialize to the on-disk OpenSSH PEM format, then load it back the
    // same way KeyLoader does for `~/.ssh` keys.
    let pem = ssh_priv
        .to_openssh(LineEnding::LF)
        .expect("serialize to OpenSSH PEM");
    assert_eq!(PemKeyFormat::detect(&pem), PemKeyFormat::OpenSsh);

    let key = LegacyPrivateKey::from_pem(&pem, None).expect("load OpenSSH PEM");
    let sig = key
        .sign(SIGNING_STRING)
        .expect("signing a file-based OpenSSH key must succeed");

    verify_signature(&public_key, "ed25519", SIGNING_STRING, &sig)
        .expect("round-tripped signature must verify");
}
