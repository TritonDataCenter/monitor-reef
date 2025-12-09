// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

use progenitor::GenerationSettings;
use std::env;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR")?;

    // OpenAPI specs are now managed by openapi-manager
    // Update this path to match your service's API name
    // Note: For clients/internal/* use ../../../openapi-specs/generated/your-api.json
    let spec_path = "../../../openapi-specs/generated/bugview-api.json";

    // Check if spec exists
    if Path::new(spec_path).exists() {
        println!("cargo:rerun-if-changed={}", spec_path);

        let spec = std::fs::read_to_string(spec_path)?;
        let openapi: openapiv3::OpenAPI = serde_json::from_str(&spec)?;

        let mut settings = GenerationSettings::default();
        settings
            .with_interface(progenitor::InterfaceStyle::Builder)
            .with_tag(progenitor::TagStyle::Merged);

        let tokens = progenitor::Generator::new(&settings).generate_tokens(&openapi)?;
        std::fs::write(format!("{}/client.rs", out_dir), tokens.to_string())?;

        println!("Generated client from OpenAPI spec: {}", spec_path);
    } else {
        eprintln!("Warning: OpenAPI spec not found at {}", spec_path);
        eprintln!("Run `cargo openapi generate` from openapi-manager to generate it");

        // Create a placeholder client
        let placeholder_client = r#"
// Placeholder client - run `cargo openapi generate` to generate the real one
pub struct Client;

impl Client {
    pub fn new(_base_url: &str) -> Self {
        Self
    }
}
"#;
        std::fs::write(format!("{}/client.rs", out_dir), placeholder_client)?;
    }

    Ok(())
}
