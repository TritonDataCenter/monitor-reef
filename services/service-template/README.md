<!--
Copyright 2025 Edgecast Cloud LLC.
This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
-->

# Service Template

This is a template for creating new Dropshot services that implement API traits.

## Usage

1. Copy the `api-template` directory to `apis/your-service-api` and define your API trait
2. Copy this directory to `services/your-service`
3. Update `Cargo.toml` with your service name and API dependency
4. Implement the API trait in `src/main.rs`
5. Add your service to the workspace `Cargo.toml`
6. Register your API in `openapi-manager/src/main.rs`

## Structure

This template demonstrates:
- How to implement an API trait
- Setting up the server with proper configuration
- Using shared state (context) for your handlers
- Logging and observability setup

## Key Differences from Function-Based Approach

### Before (Function-Based)
- Endpoints defined as free functions
- Manual API registration with `api.register(function_name)`
- Required `--openapi` flag handling for spec generation
- Tight coupling between API and implementation

### After (Trait-Based)
- Endpoints defined as trait methods
- API description generated from trait with `api_mod::api_description::<YourImpl>()`
- No manual OpenAPI handling - managed by `openapi-manager`
- Clean separation between API definition and implementation

## Benefits

- **Faster iteration**: OpenAPI specs generate without compiling implementation
- **Better testing**: Easy to create mock implementations
- **Cleaner code**: No manual spec generation logic needed
- **API versioning**: Managed automatically by dropshot-api-manager
- **Multiple implementations**: Same API, different backends
