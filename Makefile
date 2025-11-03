# Triton Rust Monorepo Makefile
# Common development commands for working with trait-based Dropshot APIs

.PHONY: help build test clean lint check format
.PHONY: api-new service-new client-new
.PHONY: service-build service-test service-run
.PHONY: client-build client-test
.PHONY: openapi-generate openapi-list openapi-check
.PHONY: dev-setup workspace-test integration-test
.PHONY: list

# Default target
help: ## Show this help message
	@echo "Triton Rust Monorepo Development Commands"
	@echo "=========================================="
	@echo ""
	@echo "Trait-Based API Architecture:"
	@echo "  1. Define API trait in apis/"
	@echo "  2. Register in openapi-manager"
	@echo "  3. Implement in services/"
	@echo "  4. Generate and commit OpenAPI specs"
	@echo "  5. Build clients (they read checked-in specs)"
	@echo ""
	@awk 'BEGIN {FS = ":.*##"} /^[a-zA-Z_-]+:.*##/ { printf "  %-20s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

# Workspace commands
build: ## Build all APIs, services and clients
	cargo build

test: ## Run all tests
	cargo test

clean: ## Clean build artifacts
	cargo clean

lint: ## Run clippy linter
	cargo clippy --all-targets --all-features -- -D warnings

check: ## Run cargo check
	cargo check --all

format: ## Format all code
	cargo fmt --all

workspace-test: ## Run all workspace tests
	cargo test --workspace

# API development commands
api-new: ## Create new API trait (usage: make api-new API=my-service-api)
	@if [ -z "$(API)" ]; then echo "Usage: make api-new API=my-service-api"; exit 1; fi
	@if [ -d "apis/$(API)" ]; then echo "API $(API) already exists"; exit 1; fi
	cp -r apis/api-template apis/$(API)
	sed -i 's/example-api/$(API)/g' apis/$(API)/Cargo.toml
	sed -i 's/ExampleApi/$(shell echo $(API) | sed 's/-/ /g' | sed 's/\b\(.\)/\u\1/g' | sed 's/ //g')/g' apis/$(API)/src/lib.rs
	@echo "Created new API: apis/$(API)"
	@echo ""
	@echo "Next steps:"
	@echo "  1. Add 'apis/$(API)' to workspace Cargo.toml members list"
	@echo "  2. Define your API types and trait in apis/$(API)/src/lib.rs"
	@echo "  3. Register in openapi-manager/src/main.rs"
	@echo "  4. Run: make openapi-generate"

# Service development commands
service-new: ## Create new service (usage: make service-new SERVICE=my-service API=my-service-api)
	@if [ -z "$(SERVICE)" ]; then echo "Usage: make service-new SERVICE=my-service API=my-service-api"; exit 1; fi
	@if [ -d "services/$(SERVICE)" ]; then echo "Service $(SERVICE) already exists"; exit 1; fi
	cp -r services/service-template services/$(SERVICE)
	sed -i 's/service-template/$(SERVICE)/g' services/$(SERVICE)/Cargo.toml
	@if [ ! -z "$(API)" ]; then \
		echo "$$API = { path = \"../../apis/$$API\" }" >> services/$(SERVICE)/Cargo.toml; \
		echo "Added dependency on $(API)"; \
	fi
	@echo "Created new service: services/$(SERVICE)"
	@echo ""
	@echo "Next steps:"
	@echo "  1. Add 'services/$(SERVICE)' to workspace Cargo.toml members list"
	@echo "  2. Implement the API trait in services/$(SERVICE)/src/main.rs"
	@echo "  3. Test: make service-run SERVICE=$(SERVICE)"

service-build: ## Build specific service (usage: make service-build SERVICE=my-service)
	@if [ -z "$(SERVICE)" ]; then echo "Usage: make service-build SERVICE=my-service"; exit 1; fi
	cargo build -p $(SERVICE)

service-test: ## Test specific service (usage: make service-test SERVICE=my-service)
	@if [ -z "$(SERVICE)" ]; then echo "Usage: make service-test SERVICE=my-service"; exit 1; fi
	cargo test -p $(SERVICE)

service-run: ## Run specific service (usage: make service-run SERVICE=my-service)
	@if [ -z "$(SERVICE)" ]; then echo "Usage: make service-run SERVICE=my-service"; exit 1; fi
	cargo run -p $(SERVICE)

# Client development commands
client-new: ## Create new client (usage: make client-new CLIENT=my-service-client API=my-api)
	@if [ -z "$(CLIENT)" ]; then echo "Usage: make client-new CLIENT=my-service-client API=my-api"; exit 1; fi
	@if [ -d "clients/internal/$(CLIENT)" ]; then echo "Client $(CLIENT) already exists"; exit 1; fi
	cp -r clients/internal/client-template clients/internal/$(CLIENT)
	sed -i 's/client-template/$(CLIENT)/g' clients/internal/$(CLIENT)/Cargo.toml
	sed -i 's/client_template/$(shell echo $(CLIENT) | tr '-' '_')/g' clients/internal/$(CLIENT)/Cargo.toml
	@if [ ! -z "$(API)" ]; then \
		sed -i 's|generated/example-api.json|generated/$(API).json|g' clients/internal/$(CLIENT)/build.rs; \
		echo "Updated build.rs to use $(API).json"; \
	fi
	@echo "Created new client: clients/internal/$(CLIENT)"
	@echo ""
	@echo "Next steps:"
	@echo "  1. Add 'clients/internal/$(CLIENT)' to workspace Cargo.toml members list"
	@echo "  2. Verify build.rs points to correct OpenAPI spec"
	@echo "  3. Run: make client-build CLIENT=$(CLIENT)"

client-build: ## Build specific client (usage: make client-build CLIENT=my-service-client)
	@if [ -z "$(CLIENT)" ]; then echo "Usage: make client-build CLIENT=my-service-client"; exit 1; fi
	cargo build -p $(CLIENT)

client-test: ## Test specific client (usage: make client-test CLIENT=my-service-client)
	@if [ -z "$(CLIENT)" ]; then echo "Usage: make client-test CLIENT=my-service-client"; exit 1; fi
	cargo test -p $(CLIENT)

# OpenAPI management commands (using dropshot-api-manager)
openapi-generate: ## Generate OpenAPI specs from API traits
	@echo "Generating OpenAPI specs using dropshot-api-manager..."
	cargo run -p openapi-manager -- generate
	@echo "OpenAPI specs generated in openapi-specs/generated/"
	@echo ""
	@echo "⚠️  Don't forget to commit the updated specs:"
	@echo "    git add openapi-specs/generated/"
	@echo "    git commit -m 'Update OpenAPI specs'"

openapi-list: ## List all managed APIs
	cargo run -p openapi-manager -- list

openapi-check: ## Check that OpenAPI specs are up-to-date (use in CI)
	cargo run -p openapi-manager -- check

openapi-debug: ## Debug OpenAPI manager configuration
	cargo run -p openapi-manager -- debug

integration-test: ## Run integration tests across all services
	cargo test --workspace integration

# Development setup
dev-setup: ## Set up development environment
	@echo "Setting up development environment..."
	@echo "Building openapi-manager..."
	cargo build -p openapi-manager
	@echo "Running initial build..."
	cargo build
	@echo "Generating OpenAPI specs..."
	$(MAKE) openapi-generate
	@echo "Running tests to ensure everything works..."
	cargo test
	@echo ""
	@echo "Development environment ready!"
	@echo ""
	@echo "Quick start:"
	@echo "  - List APIs: make list"
	@echo "  - Create API: make api-new API=my-api"
	@echo "  - Create service: make service-new SERVICE=my-service API=my-api"
	@echo "  - Generate specs: make openapi-generate"

# Quick commands for common workflows
dev: service-build service-test ## Build and test specific service (usage: make dev SERVICE=my-service)

quick-check: format lint test ## Run format, lint, and test quickly

# Full workflow for new API
new-api-workflow: ## Create complete API+Service+Client (usage: make new-api-workflow NAME=myapp)
	@if [ -z "$(NAME)" ]; then echo "Usage: make new-api-workflow NAME=myapp"; exit 1; fi
	@echo "Creating full stack for $(NAME)..."
	$(MAKE) api-new API=$(NAME)-api
	@echo ""
	@echo "Manual step: Add 'apis/$(NAME)-api' to workspace Cargo.toml and define your API trait"
	@echo "Then register it in openapi-manager/src/main.rs"
	@read -p "Press enter when ready to continue..."
	$(MAKE) openapi-generate
	$(MAKE) service-new SERVICE=$(NAME)-service API=$(NAME)-api
	$(MAKE) client-new CLIENT=$(NAME)-client API=$(NAME)-api
	@echo ""
	@echo "Created complete stack for $(NAME):"
	@echo "  API:     apis/$(NAME)-api"
	@echo "  Service: services/$(NAME)-service"
	@echo "  Client:  clients/internal/$(NAME)-client"
	@echo ""
	@echo "Next: Implement the API trait in services/$(NAME)-service/src/main.rs"

# List available APIs, services and clients
list: ## List all APIs, services and clients
	@echo "APIs:"
	@ls -1 apis/ 2>/dev/null | grep -v api-template || echo "  No APIs found"
	@echo ""
	@echo "Services:"
	@ls -1 services/ 2>/dev/null | grep -v service-template || echo "  No services found"
	@echo ""
	@echo "Internal Clients:"
	@ls -1 clients/internal/ 2>/dev/null | grep -v client-template || echo "  No internal clients found"
	@echo ""
	@echo "Managed OpenAPI Specs:"
	@ls -1 openapi-specs/generated/ 2>/dev/null || echo "  No specs generated yet (run: make openapi-generate)"

# Validation and CI commands
validate: ## Run all validation checks (CI-ready)
	@echo "Running all validation checks..."
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --workspace
	$(MAKE) openapi-check
	@echo ""
	@echo "✅ All validation checks passed!"

# Regenerate clients after OpenAPI spec changes
regen-clients: ## Regenerate all client libraries
	@echo "Regenerating clients by rebuilding..."
	cargo build
	@echo "All clients regenerated. Test with: make test"
