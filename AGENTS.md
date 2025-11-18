<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at http://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Triton Rust Monorepo Migration Guide

<!-- Note: CLAUDE.md is a symlink to this file. Edit AGENTS.md directly, not CLAUDE.md. -->

This monorepo provides a structured approach for migrating Node.js services to Rust while maintaining API compatibility through OpenAPI specifications.

## Migration Philosophy

Our migration strategy centers on **trait-based OpenAPI-driven development** to ensure exact API compatibility with fast iteration cycles:

1. **Extract OpenAPI specs** from existing Node.js services (automatically or manually)
2. **Define API traits** in separate crates for clean interface/implementation separation
3. **Build Rust services** that implement these traits using Dropshot
4. **Generate OpenAPI specs** rapidly from traits without compiling implementations
5. **Generate client libraries** using Progenitor from validated specs
6. **Validate compatibility** by comparing OpenAPI specs before deployment

## Repository Structure

```
triton-rust-monorepo/
├── apis/                    # API trait definitions (fast to compile)
│   ├── api-template/       # Template for creating new API traits
│   ├── bugview-api/        # Bugview public issue viewer API
│   └── ...                 # Add more API definitions as needed
├── services/               # Service implementations
│   ├── service-template/   # Template for trait-based services
│   ├── bugview-service/    # Bugview using external JIRA API
│   └── ...                 # Add more services as needed
├── clients/                # Client libraries
│   ├── internal/           # Clients for our trait-based APIs
│   │   ├── client-template/ # Template for generating API clients
│   │   ├── bugview-client/ # Bugview API client (Progenitor-generated)
│   │   └── jira-client/    # Client for JIRA API subset
│   └── external/           # Clients for external/legacy APIs
├── cli/                    # Command-line applications
│   └── bugview-cli/        # CLI for Bugview service
├── openapi-manager/        # OpenAPI spec management (dropshot-api-manager integration)
├── openapi-specs/          # OpenAPI specifications
│   ├── generated/          # Generated from our trait-based APIs (checked into git)
│   └── external/           # External API specs for reference (tracked in git)
└── tests/                  # Integration tests
```

## Core Technologies

- **[Dropshot](https://github.com/oxidecomputer/dropshot)**: HTTP server framework with API trait support
- **[Dropshot API Manager](https://github.com/oxidecomputer/dropshot-api-manager)**: OpenAPI document management and versioning
- **[Progenitor](https://github.com/oxidecomputer/progenitor)**: OpenAPI client generator for Rust
- **[Oxide RFD 479](https://rfd.shared.oxide.computer/rfd/0479)**: Dropshot API Traits design documentation

## Development Best Practices

### Atomic Commit Workflow

Follow this workflow for all code changes to maintain a clean, auditable history:

1. **Make changes** - Implement a single, focused change (one feature, one bug fix, one refactor)
2. **Test thoroughly** - Run all relevant tests before committing
   ```bash
   cargo test -p <your-package>
   cargo build -p <your-package>
   ```
3. **Run security audit** - Check for known vulnerabilities in dependencies
   ```bash
   # Install cargo-audit (one-time setup)
   cargo install cargo-audit

   # Run before each commit
   cargo audit

   # Update advisory database regularly
   cargo audit --update
   ```
   Review any warnings/errors and address them before committing.
4. **Update documentation** - Ensure docs reflect your changes (inline comments, README, API docs)
5. **Create atomic commit** - Commit only the changes related to this single logical change
   ```bash
   git add <relevant-files>
   git commit -m "Brief description of single change"
   ```

**Atomic Commit Guidelines:**
- One commit = one logical change
- Each commit should build and pass tests independently
- Commit messages should clearly describe what and why
- Documentation updates should be included in the same commit as the code they document

### Code Organization

**Module Structure:**
- Keep modules small and focused on a single responsibility
- Break large files into logical sub-modules
- Aim for files under 300-400 lines when possible
- Use `mod.rs` or module files to organize related functionality

**Code Reusability:**
- Extract common patterns into reusable functions/traits
- Periodically review for duplicate code and refactor
- Consider creating shared utility crates for cross-service functionality
- Document reusable components thoroughly for discoverability

**Optimization Practices:**
- Profile before optimizing (don't guess at bottlenecks)
- Regular code reviews to catch redundancy early
- Refactor incrementally - optimize as you work, not in large batches
- Balance readability with performance

## Development Workflow

### 1. Creating a New API

```bash
# 1. Copy the API template
cp -r apis/api-template apis/my-service-api
cd apis/my-service-api

# 2. Update Cargo.toml with your API name
# 3. Define your types and trait in src/lib.rs
# 4. Add to workspace Cargo.toml members list
```

Example API trait definition:

```rust
#[dropshot::api_description]
pub trait MyServiceApi {
    type Context: Send + Sync + 'static;

    #[endpoint {
        method = GET,
        path = "/resource/{id}",
        tags = ["resources"],
    }]
    async fn get_resource(
        rqctx: RequestContext<Self::Context>,
        path: Path<ResourceId>,
    ) -> Result<HttpResponseOk<Resource>, HttpError>;
}
```

### 2. Implementing a Service

```bash
# 1. Copy the service template
cp -r services/service-template services/my-service
cd services/my-service

# 2. Add dependency on your API crate in Cargo.toml
# 3. Implement the API trait in src/main.rs
# 4. Add to workspace Cargo.toml members list
```

Example implementation:

```rust
enum MyServiceImpl {}

impl MyServiceApi for MyServiceImpl {
    type Context = ApiContext;

    async fn get_resource(
        rqctx: RequestContext<Self::Context>,
        path: Path<ResourceId>,
    ) -> Result<HttpResponseOk<Resource>, HttpError> {
        // Your implementation here
    }
}

// In main():
let api = my_service_api::my_service_api_mod::api_description::<MyServiceImpl>()?;
```

See `services/bugview-service` for a complete working example.

### 3. Managing OpenAPI Specs

**Important**: Generated OpenAPI specs are checked into git in `openapi-specs/generated/`. This enables:
- Builds work without running openapi-manager first (jira-client depends on the checked-in spec)
- API changes become visible in PRs through spec diffs
- Version history tracks API evolution

```bash
# 1. Register your API in openapi-manager/src/main.rs

# 2. Generate specs (much faster than compiling implementations!):
cargo run -p openapi-manager -- generate

# 3. Review the generated spec diffs:
git diff openapi-specs/generated/

# 4. Commit the updated specs:
git add openapi-specs/generated/
git commit -m "Update OpenAPI specs for my-api changes"

# List managed APIs
cargo run -p openapi-manager -- list

# Check if specs are up-to-date (use in CI):
cargo run -p openapi-manager -- check
```

The openapi-manager uses `stub_api_description()` which generates specs without needing to compile the full service implementation. The `check` command compares generated specs against what's committed in git to catch stale specs.

### 4. Generating Clients

```bash
# 1. Copy client template
cp -r clients/internal/client-template clients/internal/my-service-client
cd clients/internal/my-service-client

# 2. Update build.rs to point to your OpenAPI spec:
#    let spec_path = "../../../openapi-specs/generated/my-api.json";

# 3. Build to generate client (reads the checked-in spec)
cargo build

# 4. Use the generated client
```

**Note**: Client build.rs reads the spec from `openapi-specs/generated/` which is checked into git. This means clients can be built without running openapi-manager first.

### 5. Building CLI Applications

Once you have a generated client library, you can build command-line tools on top of it:

```bash
# 1. Create CLI directory structure
mkdir -p cli/my-service-cli/src

# 2. Create Cargo.toml
cat > cli/my-service-cli/Cargo.toml <<EOF
[package]
name = "my-service-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "my-service"
path = "src/main.rs"

[dependencies]
my-service-client = { path = "../../clients/internal/my-service-client" }
clap = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
serde_json = { workspace = true }
EOF

# 3. Implement CLI in src/main.rs using the generated client
# 4. Add 'cli/my-service-cli' to workspace Cargo.toml members list
# 5. Build: cargo build -p my-service-cli
```

**Example**: See `cli/bugview-cli` for a complete working CLI that uses `bugview-client`.

**Benefits of this approach**:
- Type-safe client library handles all API communication
- CLI focuses on user experience (argument parsing, output formatting)
- API changes automatically flow through client regeneration
- Client library can be reused by other applications

### 6. Consuming External APIs (Interim Migration Pattern)

When building new services that need to consume external/legacy APIs during migration:

**Philosophy**: Use hand-written minimal clients instead of large auto-generated ones.

**Why?**
- Large OpenAPI specs (>1MB) often fail with Progenitor or have broken generated code
- Auto-generated clients from tools like `openapi` crate may have compilation errors
- You typically only need a small subset of endpoints
- Hand-written clients are easier to understand, debug, and maintain
- Faster compile times and smaller dependencies

**Example**: See `services/bugview-service` which consumes JIRA API

```rust
// services/my-service/src/external_client.rs
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct ExternalApiClient {
    client: Client,
    base_url: String,
}

impl ExternalApiClient {
    pub fn new(base_url: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { client, base_url })
    }

    pub async fn get_resource(&self, id: &str) -> Result<Resource> {
        let url = format!("{}/api/resource/{}", self.base_url, id);
        let response = self.client.get(&url).send().await?;
        response.json().await
    }
}
```

**Steps**:
1. Store external OpenAPI spec in `openapi-specs/external/` (tracked in git)
2. Hand-write a minimal client with only the endpoints you need
3. Define your own API trait for the service you're building
4. Implement the trait using the external client
5. Generate OpenAPI spec for YOUR API (not the external one)

**Lessons Learned**:
- Don't try to use Progenitor on massive third-party specs
- The `jira_v3_openapi` crate (1.4.1) has broken imports and doesn't compile
- Hand-writing 3-5 endpoint wrappers takes less time than debugging generated code
- This pattern works great for migration: your NEW Rust service has a clean API while consuming the OLD API internally

### 7. Testing and Validation

All services must include:
- **Unit tests** for business logic
- **Integration tests** against actual HTTP endpoints
- **OpenAPI spec validation** (automated via openapi-manager)
- **Client compatibility tests** using generated clients
- **Real data fixtures** - Sample JSON responses from actual endpoints

#### Test Data Management

Store sample JSON responses as test artifacts to ensure tests validate against real-world data:

```
tests/
├── fixtures/
│   ├── endpoint-name-success.json      # Successful response
│   ├── endpoint-name-error-404.json    # Error response
│   └── endpoint-name-list.json         # Collection response
└── integration/
    └── endpoint_tests.rs
```

**Guidelines:**
- Capture real responses from endpoints during development
- Include both success and error response examples
- Update fixtures when API response schemas change
- Use fixtures in both unit and integration tests
- Document the source/date of captured responses in comments

**Example:**
```rust
#[tokio::test]
async fn test_get_resource() {
    let fixture = include_str!("../fixtures/get-resource-success.json");
    let expected: Resource = serde_json::from_str(fixture).unwrap();
    // Test against this real data structure
}
```

**CI Check for Stale Specs**: Add this to your CI pipeline to catch when specs are out of date:

```bash
# Verify OpenAPI specs are up-to-date with trait definitions
cargo run -p openapi-manager -- check

# This will fail if:
# - API traits changed but specs weren't regenerated
# - Specs in git don't match what would be generated
```

This ensures developers remember to regenerate and commit specs when they change API traits.

#### Doctests Policy

- API trait crates (`apis/*`) and Progenitor-generated client crates include documentation examples that rustdoc treats as doctests.
- These examples are illustrative and are ignored by default in `cargo test` and CI.
- Forcing doctests to run (e.g., `cargo test -p bugview-client --doc -- --ignored`) will typically fail without a running HTTP service and async context; we intentionally do not run these in CI.
- Prefer adding runnable unit/integration tests in service crates for behavior verification.

## Migration Checklist

For each service migration:

- [ ] Create API trait in `apis/my-service-api`
- [ ] Define all types (request/response structs) with proper derives
- [ ] Define trait with `#[dropshot::api_description]` and endpoint methods
- [ ] Register API in `openapi-manager/src/main.rs`
- [ ] Generate OpenAPI spec: `cargo run -p openapi-manager -- generate`
- [ ] Review and commit spec changes: `git add openapi-specs/generated/ && git commit`
- [ ] Compare with Node.js service spec (if migrating)
- [ ] Create service implementation in `services/my-service`
- [ ] Implement the API trait with business logic
- [ ] Test service manually
- [ ] Generate client library in `clients/my-service-client`
- [ ] Write comprehensive tests (unit, integration, client)
- [ ] Document any API differences or migration notes
- [ ] Deploy to staging and validate end-to-end

## OpenAPI Manager

The `openapi-manager` crate integrates with dropshot-api-manager to provide:

- **Centralized spec management**: All OpenAPI documents in one place
- **Fast generation**: Uses stub descriptions (no need to compile implementations)
- **Automatic validation**: Ensures specs are valid and up-to-date
- **Version tracking**: For both lockstep and versioned APIs
- **Compatibility checking**: Validates backward compatibility

## Build Tooling

For direct OpenAPI management, use the openapi-manager:

```bash
cd openapi-manager
cargo run -- list                    # List all managed APIs
cargo run -- generate                # Generate all OpenAPI specs
cargo run -- check                   # Validate specs are up-to-date
```

## Configuration Management

Services should:
- Use environment variables for configuration
- Provide sane defaults for development
- Include example configuration files
- Support multiple deployment environments

## Error Handling Standards

All services must:
- Return proper HTTP status codes
- Include structured error responses in OpenAPI specs
- Log errors with appropriate detail levels
- Handle timeouts and rate limiting gracefully

## Observability

Include in all services:
- Structured logging with consistent format
- Health check endpoints
- Metrics collection points
- Distributed tracing support

## Security Considerations

- Validate all inputs according to OpenAPI specs
- Implement proper authentication/authorization
- Use secure defaults for all configurations
- Run `cargo audit` before every commit (see [Atomic Commit Workflow](#atomic-commit-workflow))
- Keep dependencies up to date with security patches
- Consider using `cargo-deny` for additional dependency policy enforcement

## Contributing

When adding new services or APIs:

1. Create API trait first (in `apis/`)
2. Register in openapi-manager
3. Implement service (in `services/`)
4. Generate and validate OpenAPI specs
5. Create client library (in `clients/`)
6. Add comprehensive tests
7. Update this documentation

## Troubleshooting

### Common Issues

**OpenAPI spec mismatch**: Check trait endpoint annotations, parameter types, and response schemas

**Client generation fails**: Verify OpenAPI spec is valid JSON and follows OpenAPI 3.0+ spec. Run `cargo run -p openapi-manager -- check` to validate.

**Build failures**: Ensure all required dependencies are in workspace Cargo.toml

**"API not found" errors**: Make sure your API is registered in `openapi-manager/src/main.rs`

**Git-related errors in openapi-manager**: For local development, use `--blessed-from-dir` flag: `cargo run -p openapi-manager -- generate --blessed-from-dir openapi-manager/openapi-specs-blessed`

### Getting Help

- Check existing service implementations (e.g., `services/bugview-service`)
- Review API trait examples (e.g., `apis/bugview-api`)
- Read [Oxide RFD 479](https://rfd.shared.oxide.computer/rfd/0479) for trait-based API design patterns
- Review [dropshot-api-manager docs](https://github.com/oxidecomputer/dropshot-api-manager)
- Consult Dropshot and Progenitor documentation

## References

- [Oxide RFD 479: Dropshot API Traits](https://rfd.shared.oxide.computer/rfd/0479)
- [Dropshot Documentation](https://github.com/oxidecomputer/dropshot)
- [Dropshot API Manager](https://github.com/oxidecomputer/dropshot-api-manager)
- [Progenitor Documentation](https://github.com/oxidecomputer/progenitor)
