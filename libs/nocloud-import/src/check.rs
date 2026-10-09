// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! A [`SourceCheck`] that compares a download with digests the caller
//! already knows.

use std::path::Path;

use anyhow::Result;

use crate::{SourceCheck, sha512_file};

/// A hash function a vendor publishes digests with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgorithm {
    Sha256,
    Sha512,
}

/// A digest the source must have, and where the caller got it (named in
/// the error if the source does not match, e.g. "the index" or
/// "SHA512SUMS at <url>").
#[derive(Debug, Clone)]
pub struct ExpectedDigest {
    pub algorithm: DigestAlgorithm,
    pub hex: String,
    pub from: String,
}

/// Accepts a source only if it matches every expected digest, so a
/// caller can require independent sources to agree. An empty list is
/// refused: accepting a source unchecked must be a deliberate choice,
/// made with a check of its own.
pub struct DigestCheck(pub Vec<ExpectedDigest>);

#[async_trait::async_trait]
impl SourceCheck for DigestCheck {
    async fn check(&self, file: &Path, sha256_hex: &str) -> Result<()> {
        anyhow::ensure!(
            !self.0.is_empty(),
            "no expected digest to check {} against",
            file.display()
        );
        let mut sha512: Option<String> = None;
        for expected in &self.0 {
            let actual = match expected.algorithm {
                DigestAlgorithm::Sha256 => sha256_hex.to_string(),
                // Hashed at most once, however many sha512s are expected.
                DigestAlgorithm::Sha512 => match &sha512 {
                    Some(hex) => hex.clone(),
                    None => {
                        let hex = sha512_file(file).await?;
                        sha512 = Some(hex.clone());
                        hex
                    }
                },
            };
            anyhow::ensure!(
                actual.eq_ignore_ascii_case(&expected.hex),
                "{} has {:?} {actual}, but {} says {}",
                file.display(),
                expected.algorithm,
                expected.from,
                expected.hex
            );
        }
        Ok(())
    }
}
