# Convert Node.js Restify API to Dropshot Trait

Convert the provided Node.js Restify API service into a Dropshot API trait definition for this Rust monorepo.

## Input

The user will provide Node.js Restify service code including:
- Route definitions (e.g., `server.get('/path/:id', handler)`)
- Handler functions
- Request/response types (if TypeScript)

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

## Checklist

Before presenting the output, verify:
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
