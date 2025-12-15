// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

use progenitor::GenerationSettings;
use std::env;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR")?;

    let spec_path = "../../../openapi-specs/generated/cloudapi-api.json";

    assert!(Path::new(spec_path).exists(), "{spec_path} does not exist!");
    println!("cargo:rerun-if-changed={}", spec_path);

    let spec = std::fs::read_to_string(spec_path)?;
    let openapi: openapiv3::OpenAPI = serde_json::from_str(&spec)?;

    let mut settings = GenerationSettings::default();
    settings
        .with_interface(progenitor::InterfaceStyle::Builder)
        .with_tag(progenitor::TagStyle::Merged)
        // Add authentication support via pre_hook_async
        // The inner_type stores AuthConfig for each request
        .with_inner_type(syn::parse_quote!(triton_auth::AuthConfig))
        // The pre_hook_async function adds Date and Authorization headers
        .with_pre_hook_async(syn::parse_quote!(crate::auth::add_auth_headers));

    let tokens = progenitor::Generator::new(&settings).generate_tokens(&openapi)?;
    std::fs::write(format!("{}/client.rs", out_dir), tokens.to_string())?;

    println!("Generated client from OpenAPI spec: {}", spec_path);
    Ok(())
}
