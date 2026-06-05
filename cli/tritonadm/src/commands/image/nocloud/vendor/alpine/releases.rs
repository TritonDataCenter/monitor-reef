// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Alpine's release metadata feed at `https://alpinelinux.org/releases.json`.
//!
//! The schema is roughly:
//!
//! ```json
//! {
//!   "latest_stable": "v3.23",
//!   "release_branches": [
//!     {
//!       "rel_branch": "v3.23",
//!       "eol_date": "2027-11-01",
//!       "releases": [
//!         { "version": "3.23.4", "date": "..." },
//!         { "version": "3.23.3", "date": "..." }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! Resolution rules for the user-facing release token:
//! - `latest` → `latest_stable` branch, newest release in it.
//! - branch (`3.23` or `v3.23`) → that branch, newest release.
//! - full version (`3.23.4`) → find the branch that contains it.

use anyhow::{Context, Result};
use serde::Deserialize;

const RELEASES_URL: &str = "https://alpinelinux.org/releases.json";

#[derive(Deserialize)]
pub struct ReleasesJson {
    pub latest_stable: String,
    pub release_branches: Vec<Branch>,
}

#[derive(Deserialize)]
pub struct Branch {
    pub rel_branch: String,
    #[serde(default)]
    pub releases: Vec<Release>,
}

#[derive(Deserialize)]
pub struct Release {
    pub version: String,
}

pub struct ResolvedRelease {
    /// Branch name including the `v` prefix, e.g. `"v3.23"`. Used as
    /// the URL path component for `dl-cdn.alpinelinux.org/alpine/<branch>/`.
    pub branch: String,
    /// Full point-release version, e.g. `"3.23.4"`. Used in the
    /// downloaded filename and as the manifest `version` field.
    pub version: String,
}

pub async fn fetch(http: &reqwest::Client) -> Result<ReleasesJson> {
    eprintln!("Fetching Alpine releases.json ...");
    http.get(RELEASES_URL)
        .send()
        .await
        .with_context(|| format!("GET {RELEASES_URL}"))?
        .error_for_status()
        .with_context(|| format!("status from {RELEASES_URL}"))?
        .json::<ReleasesJson>()
        .await
        .with_context(|| format!("parse {RELEASES_URL}"))
}

pub fn resolve(rj: &ReleasesJson, token: &str) -> Result<ResolvedRelease> {
    let token = token.trim();

    if token == "latest" {
        let branch_id = &rj.latest_stable;
        let branch = rj
            .release_branches
            .iter()
            .find(|b| &b.rel_branch == branch_id)
            .ok_or_else(|| {
                anyhow::anyhow!("latest_stable {branch_id:?} not found in release_branches")
            })?;
        let release = branch
            .releases
            .first()
            .ok_or_else(|| anyhow::anyhow!("no releases in branch {}", branch.rel_branch))?;
        return Ok(ResolvedRelease {
            branch: branch.rel_branch.clone(),
            version: release.version.clone(),
        });
    }

    // Branch token: accept "3.23" or "v3.23".
    let branch_id = if token.starts_with('v') {
        token.to_string()
    } else {
        format!("v{token}")
    };
    if let Some(branch) = rj
        .release_branches
        .iter()
        .find(|b| b.rel_branch == branch_id)
    {
        let release = branch
            .releases
            .first()
            .ok_or_else(|| anyhow::anyhow!("no releases in branch {}", branch.rel_branch))?;
        return Ok(ResolvedRelease {
            branch: branch.rel_branch.clone(),
            version: release.version.clone(),
        });
    }

    // Full version token: search all branches.
    if token.matches('.').count() == 2 {
        for branch in &rj.release_branches {
            if branch.releases.iter().any(|r| r.version == token) {
                return Ok(ResolvedRelease {
                    branch: branch.rel_branch.clone(),
                    version: token.to_string(),
                });
            }
        }
        anyhow::bail!("alpine: version {token:?} not found in any release branch");
    }

    anyhow::bail!(
        "alpine: unknown release token {token:?}; try 'latest', a branch like '3.23', \
         or a full version like '3.23.4'"
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixture() -> ReleasesJson {
        let json = r#"{
          "latest_stable": "v3.23",
          "release_branches": [
            {
              "rel_branch": "v3.23",
              "releases": [
                {"version": "3.23.4"},
                {"version": "3.23.3"}
              ]
            },
            {
              "rel_branch": "v3.22",
              "releases": [
                {"version": "3.22.6"},
                {"version": "3.22.5"}
              ]
            }
          ]
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn resolve_latest_picks_newest_release_in_latest_stable() {
        let r = resolve(&fixture(), "latest").unwrap();
        assert_eq!(r.branch, "v3.23");
        assert_eq!(r.version, "3.23.4");
    }

    #[test]
    fn resolve_branch_without_v() {
        let r = resolve(&fixture(), "3.22").unwrap();
        assert_eq!(r.branch, "v3.22");
        assert_eq!(r.version, "3.22.6");
    }

    #[test]
    fn resolve_branch_with_v() {
        let r = resolve(&fixture(), "v3.23").unwrap();
        assert_eq!(r.branch, "v3.23");
        assert_eq!(r.version, "3.23.4");
    }

    #[test]
    fn resolve_full_version_finds_branch() {
        let r = resolve(&fixture(), "3.22.5").unwrap();
        assert_eq!(r.branch, "v3.22");
        assert_eq!(r.version, "3.22.5");
    }

    #[test]
    fn resolve_unknown_branch_errors() {
        assert!(resolve(&fixture(), "3.99").is_err());
    }

    #[test]
    fn resolve_unknown_full_version_errors() {
        assert!(resolve(&fixture(), "3.23.99").is_err());
    }
}
