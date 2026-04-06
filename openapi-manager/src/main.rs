// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Edgecast Cloud LLC.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use cargo_toml::Manifest;
use clap::Parser;
use dropshot_api_manager::{Environment, ManagedApiConfig};
use dropshot_api_manager_types::{ManagedApiMetadata, Versions};
use std::process::ExitCode;

mod transforms;

fn workspace_root() -> Result<Utf8PathBuf> {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_path_buf())
        .context("openapi-manager must be in a subdirectory of the workspace root")
}

fn environment() -> Result<Environment> {
    let env = Environment::new(
        "cargo openapi".to_string(),
        workspace_root()?,
        "openapi-specs/generated",
    )?;
    Ok(env)
}

/// Read the version from a crate's Cargo.toml.
fn crate_version(crate_path: &str) -> Result<semver::Version> {
    let manifest_path = workspace_root()?.join(crate_path).join("Cargo.toml");
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
        },
        ManagedApiConfig {
            ident: "cloudapi-api",
            versions: Versions::Lockstep {
                version: crate_version("apis/cloudapi-api")?,
            },
            title: "Triton CloudAPI",
            metadata: ManagedApiMetadata {
                description: Some(
                    "Triton CloudAPI - public-facing REST API for managing virtual machines, images, networks, volumes, and other resources",
                ),
                ..ManagedApiMetadata::default()
            },
            api_description: cloudapi_api::cloud_api_mod::stub_api_description,
        },
        ManagedApiConfig {
            ident: "imgapi-api",
            versions: Versions::Lockstep {
                version: crate_version("apis/imgapi-api")?,
            },
            title: "Triton IMGAPI",
            metadata: ManagedApiMetadata {
                description: Some(
                    "Triton IMGAPI - internal HTTP API for managing virtual machine images in a Triton datacenter",
                ),
                ..ManagedApiMetadata::default()
            },
            api_description: imgapi_api::img_api_mod::stub_api_description,
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
        },
        ManagedApiConfig {
            ident: "sapi-api",
            versions: Versions::Lockstep {
                version: crate_version("apis/sapi-api")?,
            },
            title: "Triton SAPI",
            metadata: ManagedApiMetadata {
                description: Some(
                    "Triton SAPI - Services and Configuration API for managing applications, services, instances, and configuration manifests",
                ),
                ..ManagedApiMetadata::default()
            },
            api_description: sapi_api::sapi_api_mod::stub_api_description,
        },
        ManagedApiConfig {
            ident: "triton-api",
            versions: Versions::Lockstep {
                version: crate_version("apis/triton-api")?,
            },
            title: "Triton API",
            metadata: ManagedApiMetadata {
                description: Some("Triton API - public-facing HTTP API for the Triton datacenter"),
                ..ManagedApiMetadata::default()
            },
            api_description: triton_api::triton_api_mod::stub_api_description,
        },
        ManagedApiConfig {
            ident: "vmapi-api",
            versions: Versions::Lockstep {
                version: crate_version("apis/vmapi-api")?,
            },
            title: "Triton VMAPI",
            metadata: ManagedApiMetadata {
                description: Some(
                    "Triton VMAPI - internal HTTP API for managing virtual machines in a Triton datacenter",
                ),
                ..ManagedApiMetadata::default()
            },
            api_description: vmapi_api::vm_api_mod::stub_api_description,
        },
    ];
    let managed_apis = dropshot_api_manager::ManagedApis::new(apis)?;
    Ok(managed_apis)
}

fn main() -> Result<ExitCode> {
    let root = workspace_root()?;
    let generated_dir = root.join("openapi-specs/generated");
    let patched_dir = root.join("openapi-specs/patched");

    // Detect subcommand before App::parse() consumes the args
    let is_check = std::env::args().nth(1).as_deref() == Some("check");

    let app = dropshot_api_manager::App::parse();
    let env = environment()?;
    let apis = all_apis()?;

    let exit_code = app.exec(&env, &apis);

    if exit_code == ExitCode::SUCCESS {
        if is_check {
            // Verify patched specs are fresh without rewriting them
            if !transforms::check_transforms(generated_dir.as_path(), patched_dir.as_path())? {
                return Ok(ExitCode::FAILURE);
            }
        } else {
            // Regenerate patched specs from the (possibly updated) generated specs
            transforms::apply_transforms(generated_dir.as_path(), patched_dir.as_path())?;
        }
    }

    Ok(exit_code)
}
