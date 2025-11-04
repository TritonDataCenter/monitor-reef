# Triton Rust Monorepo

A structured monorepo for migrating Node.js services to Rust with guaranteed API compatibility through **trait-based OpenAPI-driven development**.

## Quick Start

```bash
# Set up development environment
make dev-setup

# See what's available
make list

# Start the bugview service
make service-run SERVICE=bugview-service

# Browse HTML or fetch JSON
open http://127.0.0.1:8080/bugview/index.html
curl http://127.0.0.1:8080/bugview/index.json | jq
```

## Key Benefits of Trait-Based Architecture

- **10x Faster Iteration** - OpenAPI generation in ~1.5s (vs 18+ seconds)
- **Clean Separation** - API definitions decoupled from implementations
- **Better Testing** - Easy to create mock implementations
- **Zero Boilerplate** - No manual `--openapi` flag handling
- **Automatic Versioning** - Built-in compatibility tracking
- **Break Circular Dependencies** - Services depend on API traits, not implementations

## Project Structure

```
triton-rust-monorepo/
├── Makefile                        # Developer commands (make help)
├── AGENTS.md                       # Comprehensive migration guide
├── apis/                           # API trait definitions (fast to compile)
│   ├── api-template/               # Template for new API traits
│   └── bugview-api/                # Bugview public issue viewer API
├── services/                       # Service implementations
│   ├── service-template/           # Template for trait-based services
│   └── bugview-service/            # Bugview service implementation
├── clients/                        # Client libraries
│   └── internal/
│       ├── client-template/        # Template for Progenitor clients
│       ├── bugview-client/         # Generated client for bugview-api
│       └── jira-client/            # Generated client for jira-api subset
├── openapi-manager/                # OpenAPI spec management
├── openapi-specs/                  # Auto-managed OpenAPI specifications
│   └── generated/                  # Specs generated from API traits
└── tests/                          # Integration tests
```

## Common Commands

```bash
# Generate OpenAPI specs (fast)
make openapi-generate

# Run a service
make service-run SERVICE=bugview-service

# Validate before committing (fmt, clippy, tests, openapi-check)
make validate

# Discover what’s available
make list
make help
```

## Architecture Overview

This repo uses Dropshot API traits (RFD 479) to separate interface from implementation and enable fast OpenAPI generation. See AGENTS.md for the complete patterns and examples.

### Trait-Based Dropshot (After)
```rust
// apis/my-api/src/lib.rs - Just the interface
#[dropshot::api_description]
pub trait MyApi {
    type Context: Send + Sync + 'static;

    #[endpoint { method = GET, path = "/issues/{key}" }]
    async fn get_issue(...) -> Result<...>;
}

// services/my-service/src/main.rs - Implementation only
enum MyServiceImpl {}

impl MyApi for MyServiceImpl {
    type Context = ApiContext;
    async fn get_issue(...) -> Result<...> {
        // implementation
    }
}

fn main() {
    // No manual OpenAPI handling needed!
    let api = my_api::my_api_mod::api_description::<MyServiceImpl>()?;
    // Start server...
}
```

## 🔍 Key Technologies

- **[Dropshot](https://github.com/oxidecomputer/dropshot)** - HTTP server framework with API trait support
- **[Dropshot API Manager](https://github.com/oxidecomputer/dropshot-api-manager)** - OpenAPI document management and versioning
- **[Progenitor](https://github.com/oxidecomputer/progenitor)** - OpenAPI client generator for Rust
- **[RFD 479](https://rfd.shared.oxide.computer/rfd/0479)** - Dropshot API Traits design documentation
- **[Schemars](https://github.com/GREsau/schemars)** - JSON Schema generation for Rust types
- **[Tokio](https://tokio.rs/)** - Async runtime
- **[Serde](https://serde.rs/)** - Serialization framework

## Example: Bugview Service

The repository includes a complete example demonstrating the trait-based workflow:

1. **API Trait** (`apis/bugview-api/`) - Interface definition with endpoint specifications
2. **Service Implementation** (`services/bugview-service/`) - Implements the BugviewApi trait
3. **Generated OpenAPI** (`openapi-specs/generated/bugview-api.json`) - Auto-generated from the trait
4. **Integration Tests** - Validates the complete workflow

```bash
# Try it out
make service-run SERVICE=bugview-service

# In another terminal, browse HTML
open http://127.0.0.1:8080/bugview/index.html

# Or fetch JSON
curl http://127.0.0.1:8080/bugview/index.json | jq
```

## Migration from Node.js

See AGENTS.md for the step‑by‑step migration workflow (API → specs → service → client), guidance, and troubleshooting.

## Documentation

See [AGENTS.md](AGENTS.md) for detailed design patterns, workflows, and troubleshooting.

## OpenAPI Management

OpenAPI specs are managed by `dropshot-api-manager` for:
- **Fast generation** - Uses `stub_api_description()` without compiling implementations
- **Automatic validation** - Ensures specs are valid and up-to-date
- **Version tracking** - For both lockstep and versioned APIs
- **Compatibility checking** - Validates backward compatibility

```bash
# Generate all specs (fast!)
make openapi-generate

# List managed APIs
make openapi-list

# Check if specs are current
make openapi-check
```

## Configuration

- Example environment variables for the Bugview service are provided at `services/bugview-service/.env.example`.
- See `services/bugview-service/README.md` for detailed configuration, endpoints, and usage.
- For local runs, export the variables or source a `.env` file before `cargo run -p bugview-service`.

## Regenerate OpenAPI and Clients

- Generate OpenAPI specs from trait crates (fast):

```bash
make openapi-generate
```

- Review and commit changes to `openapi-specs/generated/` so client builds stay deterministic:

```bash
git diff openapi-specs/generated/
git add openapi-specs/generated/
git commit -m "Update OpenAPI specs for <api>"
```

- Rebuild clients to regenerate code from updated specs:

```bash
make regen-clients
# or target a single client
make client-build CLIENT=bugview-client
```

## Tests and Doctests

- Workspace tests include unit tests, HTTP handler tests (with a mock Jira client), and spec validation.
- The API trait crates and generated client crates contain documentation examples that rustdoc treats as doctests. These are illustrative and ignored by default in `cargo test`.
- Forcing doctests to run (e.g., `cargo test -p bugview-client --doc -- --ignored`) will fail unless you provide a running service and async context. We intentionally do not run these in CI.

## Contributing

When adding new services or APIs, start with the API trait (apis/), register it in openapi-manager, generate specs, then implement the service and client. Add tests and run `make validate` before pushing.

## References

- RFD 479: Dropshot API Traits
- Dropshot (HTTP framework)
- dropshot-api-manager (OpenAPI management)
- Progenitor (client generation)

## 💡 Tips

- Use `make help` to see all available commands
- Use `make list` to see what's in your monorepo
- API trait changes require `make openapi-generate`
- After OpenAPI changes, run `make regen-clients`
- Run `make validate` before pushing to CI

This monorepo provides a robust, modern foundation for migrating Node.js services to Rust while maintaining API compatibility and ensuring type safety with 10x faster iteration cycles.
