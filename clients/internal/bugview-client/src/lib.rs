//! API Client Template
//!
//! This is a Progenitor-generated client library template.
//! When you use this template:
//! 1. Copy to clients/internal/your-service-client
//! 2. Update Cargo.toml with your service name
//! 3. Update build.rs to point to your API's OpenAPI spec
//! 4. Run cargo build to generate the client
//!
//! The generated client provides a type-safe, async interface to your API.

// Include the Progenitor-generated client code
include!(concat!(env!("OUT_DIR"), "/client.rs"));
