# Convert Node.js Restify API to Dropshot Trait

Convert a Node.js Restify API service into a Dropshot API trait definition for this Rust monorepo.

## Input

**Expect a local checkout** of the source repository. The user should provide a path like:
```
/path/to/sdc-vmapi/lib/endpoints/
```

Read the source files directly using the Read tool rather than web fetching.

Look for:
- Route definitions (e.g., `server.get('/path/:id', handler)`)
- Handler functions
- Request/response types (if TypeScript)
- API documentation in `docs/` if available

## Output

Generate Rust files for `apis/<service-name>-api/src/` containing:

1. **MPL-2.0 license header** (copy from `apis/bugview-api/src/lib.rs`)
2. **Imports** - Include all necessary imports from dropshot, schemars, serde
3. **Type definitions** - All request/response structs with proper derives
4. **API trait** - With `#[dropshot::api_description]` and endpoint methods

## File Organization for Large APIs

For small APIs (1-3 endpoints), put everything in `lib.rs`.

For large APIs with multiple endpoint groups (like VMAPI with vms.js, jobs.js, metadata.js, etc.), split into multiple files mirroring the Node.js structure:

```
apis/vmapi-api/src/
├── lib.rs          # Re-exports and main trait
├── types.rs        # Shared types used across endpoints
├── vms.rs          # VM-related types and endpoints
├── jobs.rs         # Job-related types and endpoints
├── metadata.rs     # Metadata types and endpoints
└── ping.rs         # Health check types and endpoints
```

### lib.rs Structure for Multi-File APIs

```rust
// Re-export all public types
mod types;
mod vms;
mod jobs;
mod metadata;
mod ping;

pub use types::*;
pub use vms::*;
pub use jobs::*;
pub use metadata::*;
pub use ping::*;

// Main API trait combining all endpoints
#[dropshot::api_description]
pub trait VmapiApi {
    type Context: Send + Sync + 'static;

    // VM endpoints
    #[endpoint { method = GET, path = "/vms", tags = ["vms"] }]
    async fn list_vms(...) -> ...;

    #[endpoint { method = GET, path = "/vms/{uuid}", tags = ["vms"] }]
    async fn get_vm(...) -> ...;

    // Job endpoints
    #[endpoint { method = GET, path = "/jobs", tags = ["jobs"] }]
    async fn list_jobs(...) -> ...;

    // ... etc
}
```

### Module File Structure

Each module file (e.g., `vms.rs`) contains only the types for that endpoint group:

```rust
// vms.rs - Types for VM endpoints
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path parameters for VM-specific endpoints
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VmPath {
    pub uuid: String,
}

/// Query parameters for listing VMs
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListVmsQuery {
    #[serde(default)]
    pub owner_uuid: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    // ...
}

/// VM representation
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Vm {
    pub uuid: String,
    pub alias: Option<String>,
    pub owner_uuid: String,
    // ...
}
```

### When to Split Files

Split into multiple files when:
- The Node.js code has separate endpoint files (e.g., `endpoints/vms.js`, `endpoints/jobs.js`)
- A single file would exceed ~300-400 lines
- There are distinct logical groupings of endpoints
- Types are shared across multiple endpoint groups (put in `types.rs`)

Keep in a single `lib.rs` when:
- Total endpoints ≤ 5
- All types are specific to their endpoints
- The file stays under ~300 lines

## Conventions to Follow

Reference `apis/bugview-api/src/lib.rs` as the canonical example.

### Type Definitions

- Define types BEFORE the trait
- Use these derives for response types: `#[derive(Debug, Serialize, Deserialize, JsonSchema)]`
- Use these derives for request types: `#[derive(Debug, Deserialize, JsonSchema)]`
- Add doc comments explaining each type and field
- Use `Option<T>` for optional fields with `#[serde(default)]`

### Path Parameters

Create a dedicated struct for each endpoint's path parameters:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResourcePath {
    /// Resource identifier
    pub id: String,
}
```

### Query Parameters

Create structs for query parameters:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListQuery {
    #[serde(default)]
    pub page: Option<i32>,
    #[serde(default)]
    pub limit: Option<i32>,
}
```

### Trait Definition

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
        path: Path<ResourcePath>,
    ) -> Result<HttpResponseOk<Resource>, HttpError>;
}
```

## Mapping Rules

### HTTP Methods
- `server.get(...)` → `method = GET`
- `server.post(...)` → `method = POST`
- `server.put(...)` → `method = PUT`
- `server.del(...)` → `method = DELETE`
- `server.patch(...)` → `method = PATCH`

### Route Parameters
- Restify: `/path/:id` → Dropshot: `/path/{id}`
- Restify: `/users/:userId/posts/:postId` → Dropshot: `/users/{user_id}/posts/{post_id}`

### Parameter Types
- Route params (`req.params.id`) → `Path<PathStruct>`
- Query params (`req.query.foo`) → `Query<QueryStruct>`
- Request body (`req.body`) → `TypedBody<BodyStruct>`
- No params → Just `RequestContext<Self::Context>`

### Response Types
- JSON response (always 200) → `Result<HttpResponseOk<T>, HttpError>`
- Created (201) → `Result<HttpResponseCreated<T>, HttpError>`
- No content (204) → `Result<HttpResponseDeleted, HttpError>`
- HTML response → `Result<Response<Body>, HttpError>`
- Variable status code (e.g., health checks) → `Result<Response<Body>, HttpError>`

### Type Mapping (JS → Rust)
- `string` → `String`
- `number` (integer) → `i64` or `u64`
- `number` (float) → `f64`
- `boolean` → `bool`
- `Date` / ISO string → `String` (or `chrono::DateTime<Utc>` if using chrono)
- `any` / `object` → `serde_json::Value`
- `T[]` / `Array<T>` → `Vec<T>`
- `T | null` / `T | undefined` → `Option<T>`
- `{}` (key-value objects) → `std::collections::HashMap<String, T>`

### JSON Field Naming (API Compatibility)

The original Node.js API likely uses camelCase in JSON responses. To maintain API compatibility:

1. Use snake_case for Rust field names (idiomatic Rust)
2. Add `#[serde(rename_all = "camelCase")]` on structs to serialize as camelCase
3. For individual fields that differ, use `#[serde(rename = "originalName")]`

```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VmInfo {
    pub vm_uuid: String,        // Serializes as "vmUuid"
    pub owner_uuid: String,     // Serializes as "ownerUuid"
    #[serde(rename = "RAM")]    // Override for specific field
    pub ram: u64,
}
```

### Variable HTTP Status Codes

Some endpoints return different status codes based on conditions (e.g., 200 when healthy, 503 when unhealthy). Dropshot provides several approaches:

**Option 1: Use `HttpResponseHeaders` for simple cases**
```rust
use dropshot::HttpResponseHeaders;
use http::StatusCode;

// In implementation, wrap response with custom status
HttpResponseHeaders::new_unnamed(StatusCode::SERVICE_UNAVAILABLE, response)
```

**Option 2: Use `Response<Body>` for full control**
```rust
use dropshot::Body;
use http::{Response, StatusCode};

#[endpoint {
    method = GET,
    path = "/ping",
    tags = ["health"],
}]
async fn ping(
    rqctx: RequestContext<Self::Context>,
) -> Result<Response<Body>, HttpError>;
```

**For health endpoints that may return 503**, prefer `Response<Body>` to allow the implementation to choose the status code.

### Modeling Complex/Nested Objects

When converting complex nested objects:

1. **Fully model** types that are part of the public API contract
2. **Use `serde_json::Value`** for:
   - Highly dynamic objects with unpredictable structure
   - Internal details that shouldn't be part of the API spec
   - Objects that are just passed through without inspection

```rust
// Fully modeled - stable API contract
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct VmNic {
    pub mac: String,
    pub ip: String,
    pub primary: bool,
}

// Dynamic/pass-through - use Value
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct VmDetails {
    pub uuid: String,
    /// Additional VM-specific fields (varies by brand)
    pub extra: serde_json::Value,
}
```

## Endpoint Tags

Group endpoints logically using tags:
- CRUD operations on a resource → `tags = ["resources"]`
- Health/status endpoints → `tags = ["health"]`
- HTML views → `tags = ["html"]`

## Example Conversion

### Input (Node.js Restify)
```javascript
server.get('/users/:id', async (req, res, next) => {
    const user = await getUser(req.params.id);
    res.json(user);
});

server.post('/users', async (req, res, next) => {
    const user = await createUser(req.body);
    res.status(201).json(user);
});
```

### Output (Dropshot Trait)
```rust
use dropshot::{HttpError, HttpResponseCreated, HttpResponseOk, Path, RequestContext, TypedBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UserPath {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateUserRequest {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct User {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[dropshot::api_description]
pub trait UsersApi {
    type Context: Send + Sync + 'static;

    /// Get a user by ID
    #[endpoint {
        method = GET,
        path = "/users/{id}",
        tags = ["users"],
    }]
    async fn get_user(
        rqctx: RequestContext<Self::Context>,
        path: Path<UserPath>,
    ) -> Result<HttpResponseOk<User>, HttpError>;

    /// Create a new user
    #[endpoint {
        method = POST,
        path = "/users",
        tags = ["users"],
    }]
    async fn create_user(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateUserRequest>,
    ) -> Result<HttpResponseCreated<User>, HttpError>;
}
```

## Action Dispatch Pattern

Some APIs use an action-based pattern where a single endpoint handles multiple operations via a query parameter:

```
POST /vms/:uuid?action=start
POST /vms/:uuid?action=stop
POST /vms/:uuid?action=update    (body: {ram: 1024, ...})
POST /vms/:uuid?action=add_nics  (body: {nics: [...]})
```

**See `.claude/docs/action-pattern-analysis.md` for detailed analysis.**

### Recommended Approach

1. **API trait uses `serde_json::Value` for the body** - allows any JSON
2. **Define an Action enum** for the query parameter
3. **Define typed request structs** for each action in the API crate
4. **Implementation dispatches** based on action and deserializes appropriately
5. **Client library** depends on API crate and provides typed wrapper methods

```rust
// In API trait
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VmAction {
    Start,
    Stop,
    Update,
    AddNics,
    // ... all actions
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct VmActionQuery {
    pub action: VmAction,
}

// Typed request structs (exported for client use)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateVmRequest {
    pub ram: Option<u64>,
    pub cpu_cap: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AddNicsRequest {
    pub nics: Vec<Nic>,
}

// The endpoint accepts generic JSON body
#[endpoint {
    method = POST,
    path = "/vms/{uuid}",
    tags = ["vms"],
}]
async fn vm_action(
    rqctx: RequestContext<Self::Context>,
    path: Path<VmPath>,
    query: Query<VmActionQuery>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseAccepted<JobResponse>, HttpError>;
```

### Client Library Pattern

The client crate depends on the API crate and provides typed wrappers using Progenitor's builder pattern:

```rust
// clients/internal/vmapi-client/src/lib.rs
include!(concat!(env!("OUT_DIR"), "/client.rs"));

// Re-export action enum and request types from API crate
pub use vmapi_api::{VmAction, UpdateVmRequest, AddNicsRequest, ...};

impl Client {
    pub async fn start_vm(&self, uuid: &str) -> Result<types::JobResponse, Error<types::Error>> {
        self.vm_action()
            .uuid(uuid)
            .action(vmapi_api::VmAction::Start)
            .body(serde_json::json!({}))
            .send()
            .await
            .map(|r| r.into_inner())  // Unwrap ResponseValue<T>
    }

    pub async fn update_vm(
        &self,
        uuid: &str,
        request: &vmapi_api::UpdateVmRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        self.vm_action()
            .uuid(uuid)
            .action(vmapi_api::VmAction::Update)
            .body(serde_json::to_value(request).unwrap_or_default())
            .send()
            .await
            .map(|r| r.into_inner())  // Unwrap ResponseValue<T>
    }
}
```

## Full Pipeline

After generating the API trait, complete the full pipeline to generate OpenAPI spec and client.

### Step 1: Version from package.json

Extract the version from the source project's `package.json`:

```bash
cat /path/to/source-project/package.json | grep '"version"'
# Example output: "version": "9.17.0",
```

Use this version in:
- `apis/<service>-api/Cargo.toml`
- `clients/internal/<service>-client/Cargo.toml`

### Step 2: Add API crate to workspace

Edit the root `Cargo.toml` to add the new API crate:

```toml
[workspace]
members = [
    # ... existing members
    "apis/<service>-api",
]
```

### Step 3: Create Cargo.toml for API crate

```toml
# apis/<service>-api/Cargo.toml
[package]
name = "<service>-api"
version = "<version-from-package.json>"
edition.workspace = true

[dependencies]
dropshot = { workspace = true }
schemars = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
http = "1.1"

[lints.clippy]
unused_async = "allow"
```

### Step 4: Register with OpenAPI Manager

Add the API to `openapi-manager/Cargo.toml`:

```toml
[dependencies]
# ... existing deps
<service>-api = { path = "../apis/<service>-api" }
```

Add to `openapi-manager/src/main.rs` in the `all_apis()` function:

```rust
ManagedApiConfig {
    ident: "<service>-api",
    versions: Versions::Lockstep {
        version: crate_version("apis/<service>-api")?,
    },
    title: "<Service Name> API",
    metadata: ManagedApiMetadata {
        description: Some("<Description of the API>"),
        ..ManagedApiMetadata::default()
    },
    api_description: <service>_api::<service>_api_mod::stub_api_description,
    extra_validation: None,
},
```

### Step 5: Generate OpenAPI Spec

```bash
cargo run -p openapi-manager -- generate
```

This creates `openapi-specs/generated/<service>-api.json`.

### Step 6: Create Client Crate

Create directory structure:
```
clients/internal/<service>-client/
├── Cargo.toml
├── build.rs
└── src/
    └── lib.rs
```

**Cargo.toml:**
```toml
[package]
name = "<service>-client"
version = "<version-from-package.json>"
edition.workspace = true

[lib]
name = "<service>_client"
path = "src/lib.rs"

[dependencies]
progenitor-client = { workspace = true }
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
# For APIs with action-dispatch patterns, depend on the API crate
# to re-export typed request structs
<service>-api = { path = "../../../apis/<service>-api" }

[build-dependencies]
progenitor = { workspace = true }
serde_json = { workspace = true }
openapiv3 = { workspace = true }
```

**build.rs:**
```rust
use progenitor::GenerationSettings;
use std::env;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("OUT_DIR")?;

    // OpenAPI specs are managed by openapi-manager
    let spec_path = "../../../openapi-specs/generated/<service>-api.json";

    assert!(Path::new(spec_path).exists(),
        "{spec_path} does not exist!");
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
    Ok(())
}
```

**src/lib.rs (for APIs with action-dispatch patterns):**
```rust
//! <Service Name> Client Library
//!
//! This client provides typed access to the <Service> API.
//! For action-dispatch endpoints, use the typed wrapper methods and
//! re-exported request types from the API crate.

// Include the Progenitor-generated client code
include!(concat!(env!("OUT_DIR"), "/client.rs"));

// Re-export typed request structs from the API crate.
// This allows clients to use strongly-typed requests for action-dispatch
// endpoints where the OpenAPI spec shows a generic body.
// See .claude/docs/action-pattern-analysis.md for details.
pub use <service>_api::{
    // Action enum
    <Resource>Action,

    // Action request types
    UpdateVmRequest,
    AddNicsRequest,
    // ... other action-specific request types

    // Shared types that clients may need
    Vm,
    Nic,
    // ... etc
};

// =============================================================================
// Typed Wrapper Methods for Action-Dispatch Endpoints
// =============================================================================
//
// The generated client has a generic method like:
//   vm_action(uuid, action, body: serde_json::Value) -> Result<JobResponse>
//
// These wrapper methods provide type safety at call sites.

impl Client {
    /// Start a VM
    ///
    /// # Arguments
    /// * `uuid` - The VM UUID
    pub async fn start_vm(
        &self,
        uuid: &str,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        self.vm_action()
            .uuid(uuid)
            .action(<service>_api::<Resource>Action::Start)
            .body(serde_json::json!({}))
            .send()
            .await
            .map(|r| r.into_inner())  // Unwrap ResponseValue<T>
    }

    /// Stop a VM
    ///
    /// # Arguments
    /// * `uuid` - The VM UUID
    pub async fn stop_vm(
        &self,
        uuid: &str,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        self.vm_action()
            .uuid(uuid)
            .action(<service>_api::<Resource>Action::Stop)
            .body(serde_json::json!({}))
            .send()
            .await
            .map(|r| r.into_inner())  // Unwrap ResponseValue<T>
    }

    /// Update VM properties
    ///
    /// # Arguments
    /// * `uuid` - The VM UUID
    /// * `request` - The update request with fields to modify
    pub async fn update_vm(
        &self,
        uuid: &str,
        request: &<service>_api::UpdateVmRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        self.vm_action()
            .uuid(uuid)
            .action(<service>_api::<Resource>Action::Update)
            .body(serde_json::to_value(request).unwrap_or_default())
            .send()
            .await
            .map(|r| r.into_inner())  // Unwrap ResponseValue<T>
    }

    /// Add NICs to a VM
    ///
    /// # Arguments
    /// * `uuid` - The VM UUID
    /// * `request` - The NICs to add
    pub async fn add_nics(
        &self,
        uuid: &str,
        request: &<service>_api::AddNicsRequest,
    ) -> Result<types::JobResponse, Error<types::Error>> {
        self.vm_action()
            .uuid(uuid)
            .action(<service>_api::<Resource>Action::AddNics)
            .body(serde_json::to_value(request).unwrap_or_default())
            .send()
            .await
            .map(|r| r.into_inner())  // Unwrap ResponseValue<T>
    }

    // ... add wrapper methods for all action types
}
```

**Key points for typed wrappers:**

1. **Use the builder pattern** - Progenitor generates builder-style methods (`.uuid()`, `.action()`, `.body().send()`)

2. **Unwrap ResponseValue** - Progenitor returns `Result<ResponseValue<T>, Error>`, use `.map(|r| r.into_inner())` to get `Result<T, Error>`

3. **Re-export the Action enum** - So callers can use the typed enum values

4. **Serialize request structs** - Convert typed structs to `serde_json::Value` using `serde_json::to_value(request).unwrap_or_default()`

5. **Document each wrapper** - Include doc comments explaining arguments and behavior

6. **Match the method name to the action** - e.g., `start_vm()` for `Action::Start`

7. **Request types may not implement Default** - When constructing request types in CLI code, explicitly initialize all fields rather than using `..Default::default()`

**src/lib.rs (for simple APIs without action-dispatch):**
```rust
//! <Service Name> Client Library
include!(concat!(env!("OUT_DIR"), "/client.rs"));
```

### Step 7: Add Client to Workspace

Edit root `Cargo.toml`:
```toml
members = [
    # ... existing
    "clients/internal/<service>-client",
]
```

### Step 8: Build and Verify

```bash
# Build everything
cargo build -p <service>-api
cargo run -p openapi-manager -- generate
cargo build -p <service>-client

# Verify OpenAPI spec is valid
cargo run -p openapi-manager -- check
```

### Step 9: Create CLI (Optional but Recommended)

CLIs are valuable for **validation testing** - comparing Rust client behavior against the running Node.js service. The CLI should expose **every API endpoint** so that all functionality can be tested.

Create `cli/<service>-cli/`:

**Cargo.toml:**
```toml
[package]
name = "<service>-cli"
version = "<version-from-package.json>"
edition.workspace = true

[[bin]]
name = "<service>"
path = "src/main.rs"

[dependencies]
<service>-api = { path = "../../apis/<service>-api" }
<service>-client = { path = "../../clients/internal/<service>-client" }
clap = { workspace = true, features = ["env"] }
tokio = { workspace = true }
anyhow = { workspace = true }
serde_json = { workspace = true }
```

**CLI Design Principles:**

1. **Complete coverage** - Every endpoint in the API trait should have a CLI subcommand
2. **Action subcommands** - For action-dispatch patterns, each action gets its own subcommand
3. **Raw output option** - `--raw` flag for JSON output (useful for scripting and debugging)
4. **Environment variables** - Base URL configurable via env var (e.g., `<SERVICE>_URL`)
5. **Explicit field initialization** - Request types may not implement `Default`, so initialize all fields explicitly

**Example structure for action-dispatch APIs:**
```
vmapi list [--owner-uuid UUID] [--state STATE] [--raw]
vmapi get <uuid> [--sync] [--raw]
vmapi create <json-file-or-stdin>
vmapi delete <uuid>
vmapi start <uuid> [--idempotent]
vmapi stop <uuid> [--idempotent]
vmapi reboot <uuid> [--idempotent]
vmapi kill <uuid> [--signal SIGNAL]
vmapi update <uuid> [--ram MB] [--cpu-cap PCT] ...
vmapi reprovision <uuid> --image-uuid UUID
vmapi snapshot create <uuid> [--name NAME]
vmapi snapshot rollback <uuid> --name NAME
vmapi snapshot delete <uuid> --name NAME
vmapi nic add <uuid> --network UUID
vmapi nic update <uuid> --mac MAC [--primary]
vmapi nic remove <uuid> --mac MAC
vmapi disk create <uuid> --size MB
vmapi disk resize <uuid> --pci-slot SLOT --size MB
vmapi disk delete <uuid> --pci-slot SLOT
vmapi migration list [--vm-uuid UUID] [--raw]
vmapi migration get <uuid> [--raw]
vmapi migration begin <vm-uuid> [--target-server-uuid UUID]
vmapi migration abort <uuid>
# ... etc for all endpoints
```

## Checklist

Before presenting the output, verify:

**API Trait:**
- [ ] License header is present
- [ ] All necessary imports are included (including `std::collections::HashMap` if needed)
- [ ] Types are defined before the trait
- [ ] All types have appropriate derives
- [ ] Path parameters use `{param}` syntax (not `:param`)
- [ ] Each endpoint has a doc comment
- [ ] Tags are meaningful and consistent
- [ ] Optional fields use `Option<T>` with `#[serde(default)]`
- [ ] JSON field names match original API (use `#[serde(rename_all = "camelCase")]` or `#[serde(rename = "...")]`)
- [ ] Variable status code endpoints use `Response<Body>` return type
- [ ] Complex nested objects are appropriately modeled (full types vs `serde_json::Value`)
- [ ] Action dispatch endpoints use `serde_json::Value` body with typed request structs exported

**Full Pipeline:**
- [ ] Version matches source package.json
- [ ] API crate added to workspace Cargo.toml
- [ ] API registered in openapi-manager
- [ ] OpenAPI spec generated successfully
- [ ] Client crate created with correct structure
- [ ] Client crate added to workspace Cargo.toml
- [ ] Client builds successfully

**For Action-Dispatch Patterns (if applicable):**
- [ ] Action enum exported from API crate
- [ ] Typed request structs exported from API crate (e.g., `UpdateVmRequest`, `AddNicsRequest`)
- [ ] Client crate depends on API crate
- [ ] Client re-exports action enum and request types
- [ ] Typed wrapper methods added for each action (e.g., `start_vm()`, `update_vm()`)
- [ ] Wrapper methods use builder pattern (`.uuid()`, `.action()`, `.body().send()`)
- [ ] Wrapper methods unwrap ResponseValue with `.map(|r| r.into_inner())`

**CLI (if requested):**
- [ ] CLI crate created in `cli/<service>-cli/`
- [ ] CLI added to workspace Cargo.toml
- [ ] **All API endpoints have corresponding CLI subcommands** (for validation testing against Node.js)
- [ ] All action-dispatch actions have subcommands
- [ ] `--raw` flag available for JSON output on read operations
- [ ] Environment variable support for base URL (e.g., `VMAPI_URL`)
- [ ] Helpful error messages for common failures
