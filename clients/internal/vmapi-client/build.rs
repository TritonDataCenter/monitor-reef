// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Edgecast Cloud LLC.

use progenitor::{GenerationSettings, TypePatch};
use std::env;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR")?;

    let spec_path = "../../../openapi-specs/generated/vmapi-api.json";

    assert!(Path::new(spec_path).exists(), "{spec_path} does not exist!");
    println!("cargo:rerun-if-changed={}", spec_path);

    let spec = std::fs::read_to_string(spec_path)?;
    let openapi: openapiv3::OpenAPI = serde_json::from_str(&spec)?;

    // Enum types that should derive clap::ValueEnum for CLI consumers.
    // ValueEnum's default kebab-case naming matches the serde renames
    // Progenitor generates, so no extra #[value] annotations are needed.
    let value_enum_patch = TypePatch::default().with_derive("clap::ValueEnum").clone();

    let mut settings = GenerationSettings::default();
    settings
        .with_interface(progenitor::InterfaceStyle::Builder)
        .with_tag(progenitor::TagStyle::Merged)
        // Generate JsonSchema impls so types can be used in Dropshot responses
        .with_derive("schemars::JsonSchema")
        // Add clap::ValueEnum to enum types used as CLI arguments
        .with_patch("Brand", &value_enum_patch)
        .with_patch("VmState", &value_enum_patch)
        .with_patch("MigrationState", &value_enum_patch);

    let tokens = progenitor::Generator::new(&settings).generate_tokens(&openapi)?;
    std::fs::write(format!("{}/client.rs", out_dir), tokens.to_string())?;

    println!("Generated client from OpenAPI spec: {}", spec_path);
    Ok(())
}
