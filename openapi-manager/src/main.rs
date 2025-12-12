// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use cargo_toml::Manifest;
use clap::Parser;
use dropshot_api_manager::{Environment, ManagedApiConfig};
use dropshot_api_manager_types::{ManagedApiMetadata, Versions};
use std::process::ExitCode;

fn workspace_root() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn environment() -> Result<Environment> {
    let env = Environment::new(
        "cargo openapi".to_string(),
        workspace_root(),
        "openapi-specs/generated",
    )?;
    Ok(env)
}

/// Read the version from a crate's Cargo.toml.
fn crate_version(crate_path: &str) -> Result<semver::Version> {
    let manifest_path = workspace_root().join(crate_path).join("Cargo.toml");
    let manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path))?;
    let package = manifest
        .package
        .as_ref()
        .with_context(|| format!("no [package] section in {}", manifest_path))?;
    let version_str = package
        .version
        .get()
        .with_context(|| format!("failed to resolve version in {}", manifest_path))?;
    version_str
        .parse()
        .with_context(|| format!("invalid version '{}' in {}", version_str, manifest_path))
}

fn all_apis() -> Result<dropshot_api_manager::ManagedApis> {
    let apis = vec![
        ManagedApiConfig {
            ident: "bugview-api",
            versions: Versions::Lockstep {
                version: crate_version("apis/bugview-api")?,
            },
            title: "Bugview API",
            metadata: ManagedApiMetadata {
                description: Some("Public JIRA issue viewer API"),
                ..ManagedApiMetadata::default()
            },
            api_description: bugview_api::bugview_api_mod::stub_api_description,
            extra_validation: None,
        },
        ManagedApiConfig {
            ident: "jira-api",
            versions: Versions::Lockstep {
                version: crate_version("apis/jira-api")?,
            },
            title: "JIRA API (Subset)",
            metadata: ManagedApiMetadata {
                description: Some(
                    "Subset of JIRA REST API v3 used by bugview-service. This is NOT a complete JIRA API - only the specific endpoints we consume.",
                ),
                ..ManagedApiMetadata::default()
            },
            api_description: jira_api::jira_api_mod::stub_api_description,
            extra_validation: None,
        },
    ];
    let managed_apis = dropshot_api_manager::ManagedApis::new(apis)?;
    Ok(managed_apis)
}

fn main() -> Result<ExitCode> {
    let app = dropshot_api_manager::App::parse();
    let env = environment()?;
    let apis = all_apis()?;

    Ok(app.exec(&env, &apis))
}
