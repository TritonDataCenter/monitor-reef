// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Centralized client code generator for Progenitor-based API clients.
//!
//! This tool replaces per-client build.rs scripts with a single generator
//! that produces formatted, checked-in `src/generated.rs` files for each
//! client crate.
//!
//! Each client's generation settings (patches, derives, inner_type,
//! pre_hook_async) are configured as entries in the `CLIENTS` registry.

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand};
use progenitor::{GenerationSettings, TypePatch};
use progenitor_impl::space_out_items;

/// Client code generator for the Triton Rust monorepo
#[derive(Parser)]
#[command(name = "client-generator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate all src/generated.rs files
    Generate,
    /// Check that generated files are up-to-date (for CI)
    Check,
    /// List all managed clients
    List,
}

/// Configuration for a single client crate.
struct ClientConfig {
    /// Crate name (e.g., "bugview-client")
    name: &'static str,
    /// Path to the OpenAPI spec, relative to repo root
    spec_path: &'static str,
    /// Path to the output file, relative to repo root
    output_path: &'static str,
    /// Function that configures GenerationSettings for this client
    configure: fn(&mut GenerationSettings),
}

fn configure_bugview(settings: &mut GenerationSettings) {
    let value_enum_patch = TypePatch::default().with_derive("clap::ValueEnum").clone();

    settings
        .with_interface(progenitor::InterfaceStyle::Builder)
        .with_tag(progenitor::TagStyle::Merged)
        .with_derive("schemars::JsonSchema")
        .with_patch("IssueSort", &value_enum_patch);
}

fn configure_vmapi(settings: &mut GenerationSettings) {
    let value_enum_patch = TypePatch::default().with_derive("clap::ValueEnum").clone();

    settings
        .with_interface(progenitor::InterfaceStyle::Builder)
        .with_tag(progenitor::TagStyle::Merged)
        .with_derive("schemars::JsonSchema")
        // Keep generated field types aligned with the canonical newtype
        // definitions (see vmapi-api's common / statuses modules).
        .with_replacement("Tags", "vmapi_api::Tags", std::iter::empty())
        .with_replacement(
            "MetadataObject",
            "vmapi_api::MetadataObject",
            std::iter::empty(),
        )
        .with_replacement(
            "StatusesResponse",
            "vmapi_api::StatusesResponse",
            std::iter::empty(),
        )
        .with_patch("VmBrand", &value_enum_patch)
        .with_patch("VmState", &value_enum_patch)
        .with_patch("MigrationState", &value_enum_patch)
        .with_patch("MigrationAction", &value_enum_patch);
}

fn configure_cloudapi(settings: &mut GenerationSettings) {
    let value_enum_patch = TypePatch::default().with_derive("clap::ValueEnum").clone();

    settings
        .with_interface(progenitor::InterfaceStyle::Builder)
        .with_tag(progenitor::TagStyle::Merged)
        .with_inner_type(syn::parse_quote!(triton_auth::AuthConfig))
        .with_pre_hook_async(syn::parse_quote!(crate::auth::add_auth_headers))
        .with_derive("schemars::JsonSchema")
        // Tags and MetadataObject are now named schemas (see vmapi-api's
        // newtype definitions). Replace Progenitor's generated copies
        // with type aliases to the canonical API-crate versions so field
        // types across generated structs agree with hand-written code.
        .with_replacement("Tags", "cloudapi_api::Tags", std::iter::empty())
        .with_replacement(
            "MetadataObject",
            "cloudapi_api::Metadata",
            std::iter::empty(),
        )
        .with_replacement("RoleTags", "cloudapi_api::RoleTags", std::iter::empty())
        .with_replacement(
            "ProvisioningLimits",
            "cloudapi_api::ProvisioningLimits",
            std::iter::empty(),
        )
        .with_replacement("Resolvers", "cloudapi_api::Resolvers", std::iter::empty())
        .with_replacement(
            "PolicyRules",
            "cloudapi_api::PolicyRules",
            std::iter::empty(),
        )
        .with_replacement("ImageAcl", "cloudapi_api::ImageAcl", std::iter::empty())
        .with_replacement(
            "AffinityRules",
            "cloudapi_api::AffinityRules",
            std::iter::empty(),
        )
        .with_patch("VmBrand", &value_enum_patch)
        .with_patch("Brand", &value_enum_patch)
        .with_patch("MachineState", &value_enum_patch)
        .with_patch("MachineType", &value_enum_patch)
        .with_patch("ImageState", &value_enum_patch)
        .with_patch("ImageType", &value_enum_patch)
        .with_patch("DiskState", &value_enum_patch)
        .with_patch("MigrationAction", &value_enum_patch)
        .with_patch("MigrationState", &value_enum_patch)
        .with_patch("NicState", &value_enum_patch)
        .with_patch("SnapshotState", &value_enum_patch)
        .with_patch("VolumeState", &value_enum_patch)
        .with_patch("VolumeType", &value_enum_patch)
        .with_patch("AccessKeyStatus", &value_enum_patch);
}

fn configure_jira(settings: &mut GenerationSettings) {
    settings
        .with_interface(progenitor::InterfaceStyle::Builder)
        .with_tag(progenitor::TagStyle::Merged);
}

/// Registry of all managed clients.
static CLIENTS: &[ClientConfig] = &[
    ClientConfig {
        name: "bugview-client",
        spec_path: "openapi-specs/generated/bugview-api.json",
        output_path: "clients/internal/bugview-client/src/generated.rs",
        configure: configure_bugview,
    },
    ClientConfig {
        name: "cloudapi-client",
        spec_path: "openapi-specs/patched/cloudapi-api.json",
        output_path: "clients/internal/cloudapi-client/src/generated.rs",
        configure: configure_cloudapi,
    },
    ClientConfig {
        name: "jira-client",
        spec_path: "openapi-specs/generated/jira-api.json",
        output_path: "clients/internal/jira-client/src/generated.rs",
        configure: configure_jira,
    },
    ClientConfig {
        name: "vmapi-client",
        spec_path: "openapi-specs/generated/vmapi-api.json",
        output_path: "clients/internal/vmapi-client/src/generated.rs",
        configure: configure_vmapi,
    },
];

fn repo_root() -> Result<Utf8PathBuf> {
    // Walk up from current dir looking for Cargo.toml with [workspace]
    let mut dir = Utf8PathBuf::from_path_buf(std::env::current_dir()?)
        .map_err(|p| anyhow::anyhow!("non-UTF8 path: {}", p.display()))?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents = std::fs::read_to_string(&cargo_toml)?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            bail!("could not find workspace root (no Cargo.toml with [workspace])");
        }
    }
}

fn generate_client(root: &Utf8Path, config: &ClientConfig) -> Result<String> {
    let spec_path = root.join(config.spec_path);
    anyhow::ensure!(
        spec_path.exists(),
        "spec not found: {} (run `make openapi-generate` first)",
        spec_path
    );

    let spec_str =
        std::fs::read_to_string(&spec_path).with_context(|| format!("reading {}", spec_path))?;
    let openapi: openapiv3::OpenAPI =
        serde_json::from_str(&spec_str).with_context(|| format!("parsing {}", spec_path))?;

    let mut settings = GenerationSettings::default();
    (config.configure)(&mut settings);

    let tokens = progenitor::Generator::new(&settings)
        .generate_tokens(&openapi)
        .map_err(|e| anyhow::anyhow!("generating {}: {}", config.name, e))?;

    let raw_code = tokens.to_string();
    let formatted = reformat_code(raw_code)?;

    let header = format!(
        "// Generated by client-generator from {}. Do not edit.\n\n",
        config.spec_path
    );

    Ok(format!("{}{}", header, formatted))
}

fn reformat_code(input: String) -> Result<String> {
    let formatted =
        rustfmt_wrapper::rustfmt(input).map_err(|e| anyhow::anyhow!("rustfmt failed: {}", e))?;
    space_out_items(formatted).map_err(|e| anyhow::anyhow!("space_out_items failed: {}", e))
}

fn cmd_generate() -> Result<()> {
    let root = repo_root()?;
    let mut errors = Vec::new();

    for config in CLIENTS {
        eprint!("Generating {}...", config.name);
        match generate_client(&root, config) {
            Ok(code) => {
                let output_path = root.join(config.output_path);
                // Ensure parent directory exists
                if let Some(parent) = output_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&output_path, &code)
                    .with_context(|| format!("writing {}", output_path))?;
                eprintln!(" ok");
            }
            Err(e) => {
                eprintln!(" FAILED: {}", e);
                errors.push(format!("{}: {}", config.name, e));
            }
        }
    }

    if errors.is_empty() {
        eprintln!("All {} clients generated successfully.", CLIENTS.len());
        Ok(())
    } else {
        bail!(
            "{} client(s) failed to generate:\n  {}",
            errors.len(),
            errors.join("\n  ")
        );
    }
}

fn cmd_check() -> Result<()> {
    let root = repo_root()?;
    let mut stale = Vec::new();
    let mut errors = Vec::new();

    for config in CLIENTS {
        eprint!("Checking {}...", config.name);
        match generate_client(&root, config) {
            Ok(expected) => {
                let output_path = root.join(config.output_path);
                let actual = std::fs::read_to_string(&output_path).unwrap_or_default();
                if actual == expected {
                    eprintln!(" ok");
                } else {
                    eprintln!(" STALE");
                    stale.push(config.name);
                }
            }
            Err(e) => {
                eprintln!(" FAILED: {}", e);
                errors.push(format!("{}: {}", config.name, e));
            }
        }
    }

    if !errors.is_empty() {
        bail!(
            "{} client(s) failed to generate:\n  {}",
            errors.len(),
            errors.join("\n  ")
        );
    }

    if stale.is_empty() {
        eprintln!("All {} clients are up-to-date.", CLIENTS.len());
        Ok(())
    } else {
        bail!(
            "{} client(s) have stale generated code (run `make clients-generate`):\n  {}",
            stale.len(),
            stale.join("\n  ")
        );
    }
}

fn cmd_list() -> Result<()> {
    println!("Managed clients:");
    for config in CLIENTS {
        println!(
            "  {} (spec: {}, output: {})",
            config.name, config.spec_path, config.output_path
        );
    }
    println!("\nTotal: {} clients", CLIENTS.len());
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Generate => cmd_generate(),
        Command::Check => cmd_check(),
        Command::List => cmd_list(),
    }
}
