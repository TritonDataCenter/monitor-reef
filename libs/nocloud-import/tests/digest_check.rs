// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! [`DigestCheck`] accepts a source only if it matches every expected
//! digest, so a caller can require two independent sources to agree
//! (e.g. an index and the vendor's own checksum file).

use nocloud_import::{DigestAlgorithm, DigestCheck, ExpectedDigest, SourceCheck};

const HELLO: &[u8] = b"hello\n";
const HELLO_SHA256: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
const HELLO_SHA512: &str = "e7c22b994c59d9cf2b48e549b1e24666636045930d3da7c1acb299d1c3b7f931f94aae41edda2c2b207a36e10f8bcb8d45223e54878f5b316e7ce3b6bc019629";

fn expect(algorithm: DigestAlgorithm, hex: &str, from: &str) -> ExpectedDigest {
    ExpectedDigest {
        algorithm,
        hex: hex.to_string(),
        from: from.to_string(),
    }
}

/// Run `check` against a file holding `HELLO`.
async fn run(check: DigestCheck) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("image");
    std::fs::write(&path, HELLO)?;
    check.check(&path, HELLO_SHA256).await
}

#[tokio::test]
async fn matching_sha256_and_sha512_from_two_sources_are_accepted() {
    let check = DigestCheck(vec![
        expect(DigestAlgorithm::Sha256, HELLO_SHA256, "the index"),
        expect(
            DigestAlgorithm::Sha512,
            HELLO_SHA512,
            "the vendor's SHA512SUMS",
        ),
    ]);
    run(check).await.unwrap_or_else(|e| panic!("{e:#}"));
}

#[tokio::test]
async fn upper_case_hex_is_accepted() {
    let check = DigestCheck(vec![expect(
        DigestAlgorithm::Sha256,
        &HELLO_SHA256.to_uppercase(),
        "the index",
    )]);
    run(check).await.unwrap_or_else(|e| panic!("{e:#}"));
}

#[tokio::test]
async fn one_mismatch_refuses_the_source_and_names_where_it_came_from() {
    let check = DigestCheck(vec![
        expect(DigestAlgorithm::Sha256, HELLO_SHA256, "the index"),
        expect(
            DigestAlgorithm::Sha512,
            &"0".repeat(128),
            "the vendor's SHA512SUMS",
        ),
    ]);
    let err = run(check)
        .await
        .err()
        .map(|e| format!("{e:#}"))
        .unwrap_or_default();
    assert!(err.contains("the vendor's SHA512SUMS"), "{err}");
}

#[tokio::test]
async fn no_expected_digest_is_refused_rather_than_accepted() {
    assert!(run(DigestCheck(Vec::new())).await.is_err());
}
