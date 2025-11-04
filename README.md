# Triton Rust Monorepo

A structured monorepo for migrating Node.js services to Rust with guaranteed API compatibility through **trait-based OpenAPI-driven development**.

## 🚀 Quick Start

```bash
# Set up development environment
make dev-setup

# See what's available
make list

# Start the bugview service
make service-run SERVICE=bugview-service

# Test the service
curl http://127.0.0.1:8000/health
```

## 🎯 Key Benefits of Trait-Based Architecture

- **10x Faster Iteration** - OpenAPI generation in ~1.5s (vs 18+ seconds)
- **Clean Separation** - API definitions decoupled from implementations
- **Better Testing** - Easy to create mock implementations
- **Zero Boilerplate** - No manual `--openapi` flag handling
- **Automatic Versioning** - Built-in compatibility tracking
- **Break Circular Dependencies** - Services depend on API traits, not implementations

## 📁 Project Structure

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

## 🔧 Development Workflow

### Create a Complete API Stack

```bash
# One command to create API + Service + Client
make new-api-workflow NAME=products

# Or step by step:
make api-new API=products-api
# (Add to workspace and register in openapi-manager)
make openapi-generate
make service-new SERVICE=products-service API=products-api
make client-new CLIENT=products-client API=products-api
```

### Work with Existing Services

```bash
# Build and test a service
make dev SERVICE=bugview-service

# Run a service
make service-run SERVICE=bugview-service

# Regenerate OpenAPI specs (fast!)
make openapi-generate

# List everything
make list

# Validate before committing
make validate
```

## 🛠 Makefile Commands

### API Development
- `make api-new API=name` - Create new API trait from template
- `make openapi-generate` - Generate specs using dropshot-api-manager
- `make openapi-list` - List all managed APIs
- `make openapi-check` - Validate specs are up-to-date

### Service Development
- `make service-new SERVICE=name API=api-name` - Create service with API dependency
- `make service-build SERVICE=name` - Build specific service
- `make service-test SERVICE=name` - Test specific service
- `make service-run SERVICE=name` - Run specific service

### Client Development
- `make client-new CLIENT=name API=api-name` - Create client with correct spec path
- `make client-build CLIENT=name` - Build specific client
- `make client-test CLIENT=name` - Test specific client

### Workflows
- `make dev-setup` - One-command development environment setup
- `make validate` - Run all validation checks (CI-ready)
- `make list` - List all APIs, services, clients, and specs
- `make help` - Show all available commands

## 📋 Architecture Overview

### Traditional Dropshot (Before)
```rust
// API definition mixed with implementation
#[endpoint { method = GET, path = "/issues/{key}" }]
async fn get_issue(rqctx: RequestContext<ApiContext>, ...) -> Result<...> {
    // implementation
}

fn main() {
    let mut api = ApiDescription::new();
    api.register(get_issue).unwrap();
    // Manual --openapi handling needed
}
```

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

## 🧪 Example: Bugview Service

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

## 🏁 Migration from Node.js

1. **Extract or create OpenAPI spec** from your Node.js service
2. **Define API trait** in `apis/your-service-api/`:
   ```bash
   make api-new API=your-service-api
   ```
3. **Register in openapi-manager** (add to `openapi-manager/src/main.rs`)
4. **Generate and compare specs**:
   ```bash
   make openapi-generate
   # Compare with Node.js spec
   ```
5. **Implement service**:
   ```bash
   make service-new SERVICE=your-service API=your-service-api
   # Implement the trait in src/main.rs
   ```
6. **Generate client**:
   ```bash
   make client-new CLIENT=your-service-client API=your-service-api
   ```
7. **Test everything**:
   ```bash
   make validate
   ```

## 📚 Documentation

See [AGENTS.md](AGENTS.md) for detailed information on:
- Trait-based API design patterns
- Step-by-step development workflow
- Testing strategies
- Configuration management
- Error handling standards
- Troubleshooting guide
- Before/after architecture comparison

## 🔬 OpenAPI Management

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

## 🤝 Contributing

When adding new services or APIs:

1. Create API trait first (in `apis/`) using `make api-new`
2. Register in openapi-manager
3. Generate OpenAPI specs with `make openapi-generate`
4. Implement service (in `services/`) using `make service-new`
5. Create client library (in `clients/`) using `make client-new`
6. Add comprehensive tests
7. Run `make validate` before committing

## 🎓 Learning Resources

- **[RFD 479: Dropshot API Traits](https://rfd.shared.oxide.computer/rfd/0479)** - Design philosophy and patterns
- **[Dropshot Documentation](https://github.com/oxidecomputer/dropshot)** - HTTP framework details
- **[Dropshot API Manager](https://github.com/oxidecomputer/dropshot-api-manager)** - OpenAPI management tool
- **[Progenitor Documentation](https://github.com/oxidecomputer/progenitor)** - Client generation

## 💡 Tips

- Use `make help` to see all available commands
- Use `make list` to see what's in your monorepo
- API trait changes require `make openapi-generate`
- After OpenAPI changes, run `make regen-clients`
- Run `make validate` before pushing to CI

This monorepo provides a robust, modern foundation for migrating Node.js services to Rust while maintaining API compatibility and ensuring type safety with 10x faster iteration cycles.
