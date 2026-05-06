// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Vendor profiles. Each vendor implements `VendorProfile`, which knows
//! how to resolve a release token (series name, version, or `latest`)
//! into a concrete URL, format, manifest metadata, and verifier.

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::verify::{Sha256Pinned, Verifier};

pub mod alma;
pub mod alpine;
pub mod arch;
pub mod centosstream;
pub mod custom_toml;
pub mod debian;
pub mod dirlist;
pub mod fedora;
pub mod freebsd;
pub mod omnios;
pub mod openbsd;
pub mod opensuse;
pub mod oracle;
pub mod rocky;
pub mod smartos;
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
    Alma,
    Alpine,
    Arch,
    CentosStream,
    Debian,
    Fedora,
    Freebsd,
    Omnios,
    Openbsd,
    Opensuse,
    Oracle,
    Rocky,
    Smartos,
    Talos,
    Ubuntu,
}

impl std::fmt::Display for Vendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::enum_to_display(self))
    }
}

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

/// Builder for the common "linux qcow2 with vendor-pinned sha256"
/// `ResolvedImage` shape used by the RHEL-derivative profiles
/// (alma, rocky, oracle, centosstream, fedora, opensuse). All of them
/// share `SourceFormat::Qcow2`, `os = "linux"`, `ssh_key = true`, and
/// a `Sha256Pinned` verifier driven by a hash that release discovery
/// already extracted; the only per-vendor variation is the series,
/// version, description, and homepage strings.
pub(super) struct PinnedQcow2 {
    pub url: Url,
    pub series: String,
    pub version: String,
    pub description: String,
    pub homepage: &'static str,
    pub sha256: String,
}

impl PinnedQcow2 {
    pub fn into_resolved(self, vendor_label: &str) -> Result<ResolvedImage> {
        Ok(ResolvedImage {
            url: self.url,
            format: SourceFormat::Qcow2,
            os: "linux".to_string(),
            series: self.series,
            version: self.version,
            description: self.description,
            homepage: Url::parse(self.homepage)
                .with_context(|| format!("{vendor_label} homepage url"))?,
            ssh_key: true,
            verifier: Box::new(Sha256Pinned(self.sha256.clone())),
            expected_sha256: Some(self.sha256),
        })
    }
}

pub fn lookup(vendor: Vendor) -> Box<dyn VendorProfile> {
    match vendor {
        Vendor::Alma => Box::new(alma::Alma),
        Vendor::Alpine => Box::new(alpine::Alpine),
        Vendor::Arch => Box::new(arch::Arch),
        Vendor::CentosStream => Box::new(centosstream::CentosStream),
        Vendor::Debian => Box::new(debian::Debian),
        Vendor::Fedora => Box::new(fedora::Fedora),
        Vendor::Freebsd => Box::new(freebsd::FreeBsd),
        Vendor::Omnios => Box::new(omnios::Omnios),
        Vendor::Openbsd => Box::new(openbsd::OpenBsd),
        Vendor::Opensuse => Box::new(opensuse::OpenSuse),
        Vendor::Oracle => Box::new(oracle::Oracle),
        Vendor::Rocky => Box::new(rocky::Rocky),
        Vendor::Smartos => Box::new(smartos::Smartos),
        Vendor::Talos => Box::new(talos::Talos),
        Vendor::Ubuntu => Box::new(ubuntu::Ubuntu),
    }
}
