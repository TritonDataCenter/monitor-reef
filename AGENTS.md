<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

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

**Tooling**: The `/restify-conversion` skill automates the multi-phase conversion of Node.js Restify API services to Rust Dropshot API traits. It handles route extraction, type mapping, and trait generation. Use it when starting a new service migration. See `.claude/skills/restify-conversion/SKILL.md` for details. Migration plans are stored in `conversion-plans/`.

## Repository Structure

```
triton-rust-monorepo/
├── apis/                    # API trait definitions (fast to compile)
│   ├── api-template/       # Template for creating new API traits
│   ├── bugview-api/        # Bugview public issue viewer API
│   ├── cloudapi-api/       # CloudAPI (Triton public cloud API)
│   ├── jira-api/           # JIRA integration API
│   └── vmapi-api/          # VMAPI (internal VM management API)
├── services/               # Service implementations
│   ├── service-template/   # Template for trait-based services
│   ├── bugview-service/    # Bugview using external JIRA API
│   └── jira-stub-server/   # Stub JIRA server for testing
├── clients/                # Client libraries
│   └── internal/           # Clients for our trait-based APIs
│       ├── client-template/ # Template for generating API clients
│       ├── bugview-client/ # Bugview API client (Progenitor-generated)
│       ├── cloudapi-client/ # CloudAPI client
│       ├── jira-client/    # Client for JIRA API subset
│       └── vmapi-client/   # VMAPI client
├── cli/                    # Command-line applications
│   ├── bugview-cli/        # CLI for Bugview service
│   ├── manatee-echo-resolver/ # Manatee primary resolver echo tool
│   ├── triton-cli/         # Main Triton CLI (triton command)
│   └── vmapi-cli/          # VMAPI CLI
├── client-generator/        # Progenitor-based client code generator
├── conversion-plans/        # Migration plans (cloudapi, vmapi, triton, manta-rebalancer)
├── deps/                    # Build dependencies (eng, scripts)
├── docs/                    # Design documents
├── libs/                    # Shared library crates
│   ├── cueball/            # Connection pooling framework
│   ├── cueball-*/          # Cueball resolvers and connections
│   ├── fast/               # Fast protocol library
│   ├── libmanta/           # Manta client library
│   ├── moray/              # Moray key-value store client
│   ├── rust-utils/         # Shared Rust utilities
│   └── triton-auth/        # Triton authentication library
├── openapi-manager/         # OpenAPI spec management (dropshot-api-manager integration)
├── openapi-specs/           # OpenAPI specifications
│   ├── generated/          # Generated from our trait-based APIs (checked into git)
│   └── patched/            # Post-generation patched specs (e.g., schema fixes)
└── rust/                    # Rust toolchain configuration (cargo, settings.toml)
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
2. **Format code** - Run formatter before testing to catch any formatting issues
   ```bash
   make format
   ```
3. **Test thoroughly** - Run all relevant tests before committing
   ```bash
   make package-test PACKAGE=<your-package>
   make package-build PACKAGE=<your-package>
   ```
4. **Run security audit** - Check for known vulnerabilities in dependencies
   ```bash
   # Run before each commit
   make audit

   # Update advisory database regularly
   make audit-update
   ```
   Review any warnings/errors and address them before committing.
5. **Update documentation** - Ensure docs reflect your changes (inline comments, README, API docs)
6. **Create atomic commit** - Commit only the changes related to this single logical change
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
# 1. Create new API from template
make api-new API=my-service-api

# 2. Define your types and trait in apis/my-service-api/src/lib.rs
# 3. Add to workspace Cargo.toml members list
# 4. Register in openapi-manager/src/main.rs
# 5. Generate OpenAPI spec
make openapi-generate
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
# 1. Create new service from template
make service-new SERVICE=my-service API=my-service-api

# 2. Implement the API trait in services/my-service/src/main.rs
# 3. Add to workspace Cargo.toml members list
# 4. Build and test
make service-build SERVICE=my-service
make service-test SERVICE=my-service
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
make openapi-generate

# 3. Review the generated spec diffs:
git diff openapi-specs/generated/

# 4. Commit the updated specs:
git add openapi-specs/generated/
git commit -m "Update OpenAPI specs for my-api changes"

# List managed APIs
make openapi-list

# Check if specs are up-to-date (use in CI):
make openapi-check
```

The openapi-manager uses `stub_api_description()` which generates specs without needing to compile the full service implementation. The `check` command compares generated specs against what's committed in git to catch stale specs.

### 4. Generating Clients

Client code is generated by the `client-generator` tool and checked into git as `src/generated.rs` files. This makes generated types visible to grep, IDE navigation, and code review.

```bash
# 1. Create client from template
make client-new CLIENT=my-service-client API=my-service-api

# 2. Register the client in client-generator/src/main.rs
#    Add a ClientConfig entry with spec path, output path, and generation settings
# 3. Add to workspace Cargo.toml members list
# 4. Generate client code
make clients-generate

# 5. Build to verify
make client-build CLIENT=my-service-client
```

```bash
# Regenerate all client code after OpenAPI spec changes:
make clients-generate

# Check that generated code is up-to-date (use in CI):
make clients-check
```

**Note**: Generated `src/generated.rs` files are checked into git, just like OpenAPI specs. This means clients can be built without running client-generator first.

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
edition.workspace = true

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
# 5. Build
make package-build PACKAGE=my-service-cli
```

**Example**: See `cli/bugview-cli` for a complete working CLI that uses `bugview-client`.

**Benefits of this approach**:
- Type-safe client library handles all API communication
- CLI focuses on user experience (argument parsing, output formatting)
- API changes automatically flow through client regeneration
- Client library can be reused by other applications

### 6. Action-Dispatch Pattern

Several CloudAPI endpoints use a single HTTP `POST` to handle multiple operations, with the specific operation selected via an `action` query parameter. This mirrors the original Node.js CloudAPI's Restify routes.

**How it works**: A `POST` endpoint accepts an `action` query parameter whose value is an enum variant. The request body varies depending on the action. The endpoint implementation dispatches to the correct handler based on the action value.

**Existing action-dispatch endpoints**:
- `POST /{account}/machines/{machine}?action=...` -- `MachineAction` (start, stop, reboot, resize, rename, enable/disable firewall, enable/disable deletion protection)
- `POST /{account}/images/{dataset}?action=...` -- `ImageAction` (update, export, clone, import-from-datacenter, share, unshare)
- `POST /{account}/machines/{machine}/disks/{disk}?action=...` -- `DiskAction` (resize)
- `POST /{account}/volumes/{id}?action=...` -- `VolumeAction` (update)

**Pattern structure**: Each action-dispatch endpoint has three components:

1. **Action enum** -- defines the valid actions with appropriate serde rename (e.g., `MachineAction` in `apis/cloudapi-api/src/types/machine.rs`)
2. **Action query struct** -- wraps the enum for Dropshot's `Query<>` extractor (e.g., `MachineActionQuery { action: MachineAction }`)
3. **Per-action request structs** -- separate typed bodies for each action variant (e.g., `StartMachineRequest`, `ResizeMachineRequest`)

**Body handling**: Because different actions require different body shapes, the endpoint signature uses `TypedBody<serde_json::Value>`. The implementation deserializes into the appropriate request struct after matching on the action.

```rust
// In the API trait:
async fn update_machine(
    rqctx: RequestContext<Self::Context>,
    path: Path<MachinePath>,
    query: Query<MachineActionQuery>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseOk<Machine>, HttpError>;

// In the implementation, dispatch on the action:
match query.into_inner().action {
    MachineAction::Start => { /* deserialize body as StartMachineRequest */ }
    MachineAction::Stop => { /* deserialize body as StopMachineRequest */ }
    // ...
}
```

### 7. Consuming External APIs (Interim Migration Pattern)

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

### 8. WebSocket / Channel Endpoints

Dropshot supports WebSocket endpoints via the `#[channel]` attribute. These are used for real-time streaming endpoints such as the changefeed, VNC console, and migration progress.

**Defining a channel endpoint**:

```rust
#[channel {
    protocol = WEBSOCKETS,
    path = "/{account}/changefeed",
    tags = ["changefeed"],
}]
async fn get_changefeed(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult;
```

**Key differences from REST endpoints**:
- Use `#[channel { protocol = WEBSOCKETS, ... }]` instead of `#[endpoint { ... }]`
- The last parameter must be `upgraded: WebsocketConnection`
- Return type is `WebsocketChannelResult` (not `Result<HttpResponse*, HttpError>`)
- Import `WebsocketChannelResult` and `WebsocketConnection` from `dropshot`

**Existing WebSocket endpoints** in CloudAPI:
- `/{account}/changefeed` -- real-time VM state change notifications (see `apis/cloudapi-api/src/types/changefeed.rs` for message types)
- `/{account}/migrations/{machine}/watch` -- migration progress streaming
- `/{account}/machines/{machine}/vnc` -- VNC console proxy

**Client-side consumption**: WebSocket endpoints are not covered by Progenitor-generated clients. Consumers must use a WebSocket client library (e.g., `tokio-tungstenite`) directly. See the changefeed types module for the subscription/message protocol.

**Message format**: Channel endpoints typically exchange JSON-serialized messages. Define request and response message types in the API types module so both server and client can share them. For example, `ChangefeedSubscription` (client sends) and `ChangefeedMessage` (server sends) in `apis/cloudapi-api/src/types/changefeed.rs`.

### 9. Testing and Validation

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
make openapi-check

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
- [ ] Generate OpenAPI spec: `make openapi-generate`
- [ ] Review and commit spec changes: `git add openapi-specs/generated/ && git commit`
- [ ] Compare with Node.js service spec (if migrating)
- [ ] Create service implementation in `services/my-service`
- [ ] Implement the API trait with business logic
- [ ] Test service manually
- [ ] Generate client library: register in `client-generator/src/main.rs`, run `make clients-generate`
- [ ] Review and commit generated code: `git add clients/internal/my-service-client/src/generated.rs`
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

For OpenAPI management, use the make targets:

```bash
make openapi-list      # List all managed APIs
make openapi-generate  # Generate all OpenAPI specs
make openapi-check     # Validate specs are up-to-date
make openapi-debug     # Debug OpenAPI manager configuration
```

## Common Make Targets

Run `make help` to see all available targets. Key commands:

| Target | Description |
|--------|-------------|
| `make build` | Build all crates |
| `make test` | Run all tests |
| `make check` | Run all validation (tests + OpenAPI check) |
| `make format` | Format all code |
| `make lint` | Run clippy linter |
| `make audit` | Security audit dependencies |
| `make list` | List all APIs, services, and clients |

### Package-specific commands

| Target | Description |
|--------|-------------|
| `make package-build PACKAGE=X` | Build specific package |
| `make package-test PACKAGE=X` | Test specific package |
| `make service-run SERVICE=X` | Run a service |

### OpenAPI commands

| Target | Description |
|--------|-------------|
| `make openapi-generate` | Generate specs from API traits |
| `make openapi-check` | Verify specs are up-to-date |
| `make openapi-list` | List managed APIs |

### Client generation commands

| Target | Description |
|--------|-------------|
| `make clients-generate` | Generate all client `src/generated.rs` files |
| `make clients-check` | Verify generated client code is up-to-date |
| `make clients-list` | List managed clients |
| `make regen-clients` | Regenerate OpenAPI specs + client code |

### Scaffolding commands

| Target | Description |
|--------|-------------|
| `make api-new API=X` | Create new API trait crate |
| `make service-new SERVICE=X API=Y` | Create new service |
| `make client-new CLIENT=X API=Y` | Create new client |

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

## Type Safety Rules

These rules prevent type-safety issues in CLI and client code. They are enforced by the `/type-safety-audit` skill (see `.claude/commands/type-safety-audit.md`) and should be followed in all new code. Violations found by the audit should be filed as beads issues with the `type-safety` label (see [Issue Tracking with Beads](#issue-tracking-with-beads)).

### 1. No Hardcoded Enum Strings

Never use string literals that match enum variant wire names. Use `enum_to_display()` or direct enum comparison.

```rust
// WRONG: hardcoded string matching an enum variant
if state_str == "running" { ... }

// RIGHT: compare typed enums directly
if machine.state == MachineState::Running { ... }

// RIGHT: use enum_to_display() when you need the wire-format string
println!("State: {}", enum_to_display(&machine.state));
```

### 2. ValueEnum on API Types

If an enum is used as a CLI argument (a `clap::Args` field), it **must** have `clap::ValueEnum` derived on the canonical API type definition in `apis/*/src/types/`. Progenitor generates separate types — the derive must be on the source type if the CLI imports via re-export.

### 3. client-generator Patch Consistency

Every enum used as a CLI argument must also have `with_patch(EnumName, &value_enum_patch)` in the corresponding client's configuration in `client-generator/src/main.rs`. This ensures the Progenitor-generated copy also gets `ValueEnum` for cases where the CLI uses `types::EnumName`.

### 4. No Duplicate Enum Definitions

Never reimplement enums that exist in API types or are generated by Progenitor. Import from `<service>_client::types::*` (Progenitor types) or the re-exported API types.

### 5. Forward Compatibility

Enums deserializing untrusted or evolving input (state fields, status fields) must include a `#[serde(other)] Unknown` catch-all variant. See `apis/cloudapi-api/src/types/changefeed.rs` for the established pattern.

### 6. Re-export Pattern

CLIs import types from `<service>_client` re-exports, not directly from API crates. The client crate re-exports canonical API types alongside Progenitor-generated types in `src/lib.rs`.

### 7. No Debug Format for User-Facing Output

Never use `{:?}` (Debug format) for values shown to users. Use `enum_to_display()` for serde enums, `.join(", ")` for collections, or implement `Display`. Debug format exposes Rust internals (e.g., `Brand::Bhyve` instead of `bhyve`).

```rust
// WRONG: Debug format in user-facing output
println!("  Brand: {:?}", brand);
println!("Waiting for {:?}", target_names);

// RIGHT: use enum_to_display() for serde enums
println!("  Brand: {}", enum_to_display(brand));

// RIGHT: use .join() for collections
println!("Waiting for {}", target_names.join(", "));
```

### 8. Field Naming Exceptions

Most CloudAPI response structs use `#[serde(rename_all = "camelCase")]` to match the JSON wire format. However, some fields from the original Node.js CloudAPI are returned in snake_case or other non-camelCase formats. These must use explicit `#[serde(rename = "...")]` overrides.

**Known exceptions in `Machine` (camelCase struct)**:
- `dns_names` -- returned as `"dns_names"` (snake_case) by CloudAPI despite other fields being camelCase
- `free_space` -- returned as `"free_space"` (snake_case) for bhyve flexible disk VMs
- `delegate_dataset` -- returned as `"delegate_dataset"` (snake_case)

**Other common rename patterns**:
- `type` fields -- Rust reserves `type` as a keyword, so fields like `machine_type` and `volume_type` use `#[serde(rename = "type")]`
- `role-tag` fields -- CloudAPI uses hyphenated `"role-tag"` in JSON, mapped to `role_tag` in Rust with `#[serde(rename = "role-tag")]`
- Enum variants with hyphens -- e.g., `#[serde(rename = "joyent-minimal")]`, `#[serde(rename = "zone-dataset")]`

**When adding new fields**: Always check the actual JSON wire format from the original Node.js service. If a field does not follow the struct-level `rename_all` convention, add an explicit `#[serde(rename = "...")]` with a comment explaining the exception.

```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: Uuid,
    pub name: String,
    // ...
    /// Note: CloudAPI returns this as snake_case despite other fields being camelCase
    #[serde(rename = "dns_names", default, skip_serializing_if = "Option::is_none")]
    pub dns_names: Option<Vec<String>>,
}
```

## UUID Handling Conventions

UUIDs are pervasive throughout the Triton APIs. Follow these conventions for consistency:

**Type alias**: API crates define `pub type Uuid = uuid::Uuid;` in their common types module (see `apis/cloudapi-api/src/types/common.rs`). Use this alias in all struct fields and function signatures rather than raw `uuid::Uuid` or `String`.

**Serialization**: The `uuid` crate's serde support handles serialization/deserialization as lowercase hyphenated strings (e.g., `"28faa36c-2031-4632-a819-f7defa1299a3"`). No custom serde logic is needed.

**Path parameters**: UUID path parameters (machine IDs, image IDs, etc.) are parsed automatically by Dropshot via the `Uuid` type in `Path<>` structs. Invalid UUIDs produce a 400 error.

**String UUIDs**: Some fields use `String` instead of `Uuid` when the upstream API may return non-UUID values or when the field serves double duty (e.g., `ChangefeedSubscription.vms` accepts UUIDs as strings). Prefer typed `Uuid` unless there is a specific reason for `String`.

**Testing**: When constructing test UUIDs, use `uuid::Uuid::parse_str("...")` or `uuid::Uuid::nil()` rather than placeholder strings.

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

**Client generation fails**: Verify OpenAPI spec is valid JSON and follows OpenAPI 3.0+ spec. Run `make openapi-check` to validate.

**Build failures**: Ensure all required dependencies are in workspace Cargo.toml

**"API not found" errors**: Make sure your API is registered in `openapi-manager/src/main.rs`

**Git-related errors in openapi-manager**: For local development, use `--blessed-from-dir` flag: `cargo run -p openapi-manager -- generate --blessed-from-dir openapi-manager/openapi-specs-blessed`

### Getting Help

- Check existing service implementations (e.g., `services/bugview-service`)
- Review API trait examples (e.g., `apis/bugview-api`)
- Read [Oxide RFD 479](https://rfd.shared.oxide.computer/rfd/0479) for trait-based API design patterns
- Review [dropshot-api-manager docs](https://github.com/oxidecomputer/dropshot-api-manager)
- Consult Dropshot and Progenitor documentation

## Issue Tracking with Beads

This repo uses [Beads](https://github.com/steveyegge/beads) (`bd` CLI) for lightweight issue tracking. Issues are stored in `.beads/` with JSONL export tracked in git. Type safety findings from the `/type-safety-audit` skill (see [Type Safety Rules](#type-safety-rules)) should be tracked as beads issues with the `type-safety` label.

### Core Workflow

```bash
# See what's ready to work on
bd ready

# Claim an issue (sets you as assignee, marks in-progress)
bd update <id> --claim

# View issue details
bd show <id>

# Close an issue after fixing
bd close <id>

# Close as won't-fix (add a comment explaining why)
bd comments add <id> "Reason for not fixing..."
bd close <id> -r "wontfix: brief summary"

# Create a new issue
bd create --title "Short description" --description "Details" --add-label type-safety
```

### Session Convention

When working on tracked items, check `bd ready` at session start to see the current work queue. When finishing work:

1. Add comments with reasoning using `bd comments add <id> "..."`, especially for won't-fix closures
2. Close the issue with `bd close <id>` (use `-r` for a short reason)
3. Create an atomic commit that includes both the code changes and the updated `.beads/issues.jsonl`
4. Create new issues for any follow-up work discovered

### MCP Integration (Optional)

For richer integration, configure `beads-mcp` in your personal `.claude/settings.local.json`:

```json
{
  "mcpServers": {
    "beads": {
      "command": "beads-mcp",
      "args": ["--db", ".beads/beads.db"]
    }
  }
}
```

This is per-user opt-in and not required for the basic `bd` CLI workflow.

## References

- [Oxide RFD 479: Dropshot API Traits](https://rfd.shared.oxide.computer/rfd/0479)
- [Dropshot Documentation](https://github.com/oxidecomputer/dropshot)
- [Dropshot API Manager](https://github.com/oxidecomputer/dropshot-api-manager)
- [Progenitor Documentation](https://github.com/oxidecomputer/progenitor)
