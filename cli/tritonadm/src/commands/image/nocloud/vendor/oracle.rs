// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Oracle Linux cloud-image vendor profile.
//!
//! Oracle's cloud-init enabled KVM templates live at
//! `https://yum.oracle.com/templates/OracleLinux/OL<n>/u<u>/x86_64/`,
//! one `OL<n>U<u>_x86_64-kvm-b<build>.qcow2` per release. There are
//! no per-image checksum sidecars; sha256s are embedded in the
//! `https://yum.oracle.com/oracle-linux-templates.html` landing
//! page's table, paired with image links per `<tr>` row. The
//! release-resolution path scrapes that page once at metadata time
//! to extract the (url, sha256, version) tuple, so the verifier is
//! a `Sha256Pinned` and `--dry-run` shows the manifest UUID.

mod releases;

use anyhow::{Context, Result};
use async_trait::async_trait;
use url::Url;

use super::{PinnedQcow2, ResolvedImage, VendorProfile};

pub struct Oracle;

#[async_trait]
impl VendorProfile for Oracle {
    fn name(&self) -> &str {
        "oracle"
    }

    async fn resolve(&self, release: &str, http: &reqwest::Client) -> Result<ResolvedImage> {
        let resolved = releases::resolve(http, release).await?;
        let url: Url = resolved.url.parse().context("oracle image url")?;
        PinnedQcow2 {
            url,
            series: format!("oracle{}", resolved.major),
            version: resolved.version(),
            description: format!(
                "Oracle Linux {}.{} CloudInit NoCloud compatible image. \
                 Built to run on bhyve virtual machines.",
                resolved.major, resolved.update
            ),
            homepage: "https://www.oracle.com/linux/",
            sha256: resolved.sha256,
        }
        .into_resolved("oracle")
    }
}
