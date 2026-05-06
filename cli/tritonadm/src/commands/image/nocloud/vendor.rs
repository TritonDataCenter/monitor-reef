// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Vendor profiles. Each vendor implements `VendorProfile`, which knows
//! how to resolve a release token (series name, version, or `latest`)
//! into a concrete URL, format, manifest metadata, and verifier.

use anyhow::Result;
use async_trait::async_trait;
use url::Url;

use super::verify::Verifier;

pub mod alpine;
pub mod debian;
pub mod fedora;
pub mod freebsd;
pub mod talos;
pub mod ubuntu;

/// Built-in vendor profiles. Driven by clap's `ValueEnum` so the CLI
/// help auto-lists supported vendors and validates the argument
/// before any I/O. The variant→string mapping is derived from
/// `serde::Serialize` (kebab-case), and `Display` delegates to
/// `crate::enum_to_display`, so adding a vendor is a single-line
/// enum-variant addition with no string-matching boilerplate.
#[derive(clap::ValueEnum, serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Vendor {
    Alpine,
    Debian,
    Fedora,
    Freebsd,
    Talos,
    Ubuntu,
}

impl std::fmt::Display for Vendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::enum_to_display(self))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum SourceFormat {
    Qcow2,
    Xz,
    // Raw is wired through the pipeline but not currently emitted by
    // any vendor — kept so that future raw-image vendors don't need to
    // touch the convert step.
    #[allow(dead_code)]
    Raw,
}

pub struct ResolvedImage {
    pub url: Url,
    pub format: SourceFormat,
    /// Image OS for the manifest (`linux`, `bsd`, ...).
    pub os: String,
    /// Canonical short release name (e.g. `noble`). Used in output
    /// filenames and the manifest `name` field.
    pub series: String,
    /// Vendor-chosen version string (often a date stamp). Used as the
    /// manifest `version` field.
    pub version: String,
    pub description: String,
    pub homepage: Url,
    pub ssh_key: bool,
    pub verifier: Box<dyn Verifier>,
    /// Vendors that get the sha256 from their metadata feed (e.g.
    /// Ubuntu Simple Streams) populate this so `--dry-run` can show
    /// the expected hash and the derived manifest UUID without
    /// downloading anything. Vendors whose verifier fetches the hash
    /// at verification time leave this `None`.
    pub expected_sha256: Option<String>,
}

#[async_trait]
pub trait VendorProfile: Send + Sync {
    // Reserved for diagnostics/logging by future callers; not yet
    // referenced by the POC dispatcher.
    #[allow(dead_code)]
    fn name(&self) -> &str;
    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage>;
}

pub fn lookup(vendor: Vendor) -> Box<dyn VendorProfile> {
    match vendor {
        Vendor::Alpine => Box::new(alpine::Alpine),
        Vendor::Debian => Box::new(debian::Debian),
        Vendor::Fedora => Box::new(fedora::Fedora),
        Vendor::Freebsd => Box::new(freebsd::FreeBsd),
        Vendor::Talos => Box::new(talos::Talos),
        Vendor::Ubuntu => Box::new(ubuntu::Ubuntu),
    }
}
