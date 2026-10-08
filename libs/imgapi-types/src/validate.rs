// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Checks on an image manifest that the wire schema cannot express.
//!
//! The types themselves accept any manifest an IMGAPI server may serve.
//! Tools that act on a manifest (installing an image, writing one) call
//! [`Image::validate`] to refuse ones they should not trust. The checks
//! match smartos-live's `imgapi-manifest` crate.

use super::image::{Image, ImageState};

/// The only manifest version this crate understands. Version 1
/// manifests have not been produced since 2014.
pub const MANIFEST_VERSION: u32 = 2;

/// Why a manifest failed [`Image::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("manifest schema version is {got}; expected {expected}")]
    SchemaMismatch { got: u32, expected: u32 },
    #[error("files[{index}].sha1 must be 40 lowercase hex chars, got {got:?}")]
    BadSha1 { index: usize, got: String },
    #[error("manifest has zero files but state is active")]
    NoFilesForActive,
}

impl Image {
    /// Check what serde cannot: the manifest version is
    /// [`MANIFEST_VERSION`], every file's `sha1` is 40 lowercase hex
    /// characters, and an active image has at least one file.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.v != MANIFEST_VERSION {
            return Err(ManifestError::SchemaMismatch {
                got: self.v,
                expected: MANIFEST_VERSION,
            });
        }
        for (index, file) in self.files.iter().enumerate() {
            let ok = file.sha1.len() == 40
                && file
                    .sha1
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
            if !ok {
                return Err(ManifestError::BadSha1 {
                    index,
                    got: file.sha1.clone(),
                });
            }
        }
        if self.state == ImageState::Active && self.files.is_empty() {
            return Err(ManifestError::NoFilesForActive);
        }
        Ok(())
    }
}
