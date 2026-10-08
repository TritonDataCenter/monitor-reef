// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Build a SmartOS bhyve image from a vendor's NoCloud cloud image:
//! download and check the source, decode it onto a zvol, snapshot and
//! send it, compress the stream, and write its IMGAPI manifest. The
//! result can be installed with `imgadm install` or pushed to IMGAPI.
//!
//! Which image to build, and how far to trust the download, are the
//! caller's: see [`Source`], [`ImageInfo`] and [`SourceCheck`].

pub mod manifest;
mod pipeline;
pub mod zfs;

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256, Sha512};
use tokio::io::AsyncReadExt;

pub use pipeline::*;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    Qcow2,
    Xz,
    Raw,
    /// VMDK (VMware Virtual Disk). Used by OmniOS's cloud images.
    /// The release-resolution path is wired up; the conversion step
    /// is deferred pending a vendored vmdk reader.
    Vmdk,
    /// gzipped raw disk image. Used by SmartOS
    /// (`smartos-<rel>-USB.img.gz`). The pipeline streams a
    /// gzip decoder straight into the zvol, no intermediate file.
    RawGz,
}

pub async fn sha256_file(file: &Path) -> Result<String> {
    hash_file::<Sha256>(file).await
}

pub async fn sha512_file(file: &Path) -> Result<String> {
    hash_file::<Sha512>(file).await
}

async fn hash_file<H: Digest>(file: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(file)
        .await
        .with_context(|| format!("open {}", file.display()))?;
    let mut hasher = H::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format_hex(&hasher.finalize()))
}

fn format_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}
