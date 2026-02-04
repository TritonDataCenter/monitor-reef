<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Action-Based Endpoint Pattern Analysis

## Problem Statement

The VMAPI service uses an action dispatch pattern where a single endpoint handles multiple operations distinguished by a query parameter:

```
POST /vms/:uuid?action=start        (minimal/no body)
POST /vms/:uuid?action=stop         (minimal/no body)
POST /vms/:uuid?action=update       (body: {ram: 1024, cpu_cap: 200, ...})
POST /vms/:uuid?action=add_nics     (body: {nics: [{...}, {...}]})
POST /vms/:uuid?action=create_snapshot (body: {name: "snap1"})
... 16 total actions
```

This pattern presents challenges when migrating to Rust with our toolchain:
- **Dropshot**: Routes on path + method; single endpoint must handle all actions
- **OpenAPI**: No native support for "if query param X=Y, use body schema Z"
- **Progenitor**: Client quality depends heavily on OpenAPI spec quality

The constraint is fixed: **we cannot change the API structure** due to compatibility requirements.

## Analysis of Technical Constraints

### OpenAPI Specification Limitations

According to the [OpenAPI Specification](https://spec.openapis.org/oas/v3.1.0), paths must be unique up to the query string separator (`?`). This means `/vms/{uuid}?action=start` and `/vms/{uuid}?action=stop` are the same path and cannot have separate definitions.

The specification offers `oneOf` with `discriminator` for polymorphic bodies, but the discriminator **must be a property within the request body itself** - it cannot reference a query parameter. From the [Swagger documentation on discriminators](https://swagger.io/docs/specification/v3_0/data-models/inheritance-and-polymorphism/):

> "When used, the discriminator will be the name of the property that decides which schema definition validates the structure of the model."

This is a fundamental mismatch with action-dispatch patterns where the discriminator lives outside the body.

### Dropshot's Design

Dropshot endpoints accept a single body type per endpoint. From the [Dropshot documentation](https://docs.rs/dropshot/latest/dropshot/index.html):

> "TypedBody<J> extracts content from the request body by parsing the body as JSON and deserializing it into an instance of type J."

While you can use a `#[serde(untagged)]` enum as J, this has significant drawbacks (discussed below).

### Progenitor's Limitations

[Progenitor](https://github.com/oxidecomputer/progenitor) generates clients from OpenAPI specs. Its handling of `oneOf` schemas has historically been problematic:

- May generate `serde_json::Value` instead of proper enums
- Untagged enum deserialization can fail silently or with poor error messages
- The generated client method signature reflects the OpenAPI spec's ambiguity

## Options Analysis

### Option A: `serde_json::Value` Body with Per-Action Validation

**Approach:**
```rust
// In API trait (apis/vmapi-api/src/lib.rs)
#[endpoint { method = POST, path = "/vms/{uuid}" }]
async fn vm_action(
    rqctx: RequestContext<Self::Context>,
    path: Path<VmPath>,
    query: Query<ActionQuery>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseOk<VmActionResponse>, HttpError>;

// Define typed structs in the same crate for documentation
pub struct UpdateVmBody { pub ram: Option<u64>, pub cpu_cap: Option<u32>, ... }
pub struct AddNicsBody { pub nics: Vec<Nic>, ... }
pub struct CreateSnapshotBody { pub name: String }
```

**Implementation side:**
```rust
async fn vm_action(...) -> Result<...> {
    let action = query.into_inner().action;
    let body = body.into_inner();

    match action.as_str() {
        "start" | "stop" => {
            // Validate body is empty or minimal
            handle_lifecycle_action(uuid, action).await
        }
        "update" => {
            let update: UpdateVmBody = serde_json::from_value(body)
                .map_err(|e| HttpError::for_bad_request(None, e.to_string()))?;
            handle_update(uuid, update).await
        }
        // ... etc
    }
}
```

**OpenAPI output:** Shows `object` or `{}` as body schema - loses type information.

**Client generation:** Generic method accepting `serde_json::Value`.

**Pros:**
- Simple to implement
- Flexible - handles any body structure
- Clear separation between API contract and validation

**Cons:**
- No compile-time type safety for clients
- Poor API documentation in OpenAPI spec
- Clients must hand-write typed wrappers
- Runtime validation errors instead of compile-time

---

### Option B: `#[serde(untagged)]` Enum Body

**Approach:**
```rust
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum VmActionBody {
    Empty {},
    Update(UpdateVmBody),
    AddNics(AddNicsBody),
    CreateSnapshot(CreateSnapshotBody),
    // ... all 16 variants
}
```

**Pros:**
- Single Rust type captures all possibilities
- OpenAPI spec shows `oneOf` with all schemas
- Some type information preserved

**Cons:**
- **Order-dependent deserialization**: Serde tries variants in order; first match wins
- **Overlapping schemas cause bugs**: If `UpdateVmBody` and `AddNicsBody` have any common fields, wrong variant may deserialize
- **Terrible error messages**: From [serde documentation](https://serde.rs/enum-representations.html): "When no variant matches, untagged does not produce an informative error"
- **No connection to action query param**: Body could be `CreateSnapshotBody` while action is `start`
- **Progenitor may struggle**: Complex `oneOf` without discriminator often generates poor code

**Verdict:** Not recommended due to fragility and poor error handling.

---

### Option C: Internally Tagged Enum (Discriminator in Body)

**Approach:** Require clients to include the action in the body as well as (or instead of) the query param.

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(tag = "action")]
pub enum VmActionBody {
    #[serde(rename = "start")]
    Start {},
    #[serde(rename = "update")]
    Update { ram: Option<u64>, cpu_cap: Option<u32> },
    #[serde(rename = "add_nics")]
    AddNics { nics: Vec<Nic> },
    // ...
}
```

**Pros:**
- Clean Rust types with compile-time exhaustiveness
- OpenAPI `oneOf` with proper `discriminator`
- Progenitor generates proper enum
- Excellent error messages on invalid input

**Cons:**
- **Breaks API compatibility**: Existing clients send action in query param only
- Requires body even for no-body actions like `start`/`stop`

**Verdict:** Would be ideal for a greenfield API, but **violates our compatibility constraint**.

---

### Option D: Client Library Depends on API Crate (Recommended Hybrid)

**Approach:**
1. API trait uses `serde_json::Value` body for maximum flexibility
2. API crate also exports all typed request structs with full documentation
3. Progenitor generates a generic client method
4. Client crate adds typed wrapper methods that use the API crate's types

```rust
// apis/vmapi-api/src/lib.rs

/// Request body for the `update` action
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateVmRequest {
    /// RAM in megabytes
    pub ram: Option<u64>,
    /// CPU cap percentage (0-100)
    pub cpu_cap: Option<u32>,
    // ... other fields with full documentation
}

/// Request body for the `add_nics` action
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddNicsRequest {
    /// NICs to add to the VM
    pub nics: Vec<Nic>,
}

// The trait endpoint accepts generic JSON
#[endpoint { method = POST, path = "/vms/{uuid}" }]
async fn vm_action(
    rqctx: RequestContext<Self::Context>,
    path: Path<VmPath>,
    query: Query<ActionQuery>,
    body: TypedBody<serde_json::Value>,
) -> Result<HttpResponseOk<VmActionResponse>, HttpError>;
```

```rust
// clients/internal/vmapi-client/src/lib.rs

// Re-export the Progenitor-generated client
pub use generated::Client;

// Re-export types from the API crate
pub use vmapi_api::{
    VmAction, UpdateVmRequest, AddNicsRequest, CreateSnapshotRequest, ...
};

// Wrapper struct - cannot impl on Progenitor's Client directly
pub struct TypedClient {
    inner: Client,
}

impl TypedClient {
    pub fn new(base_url: &str) -> Self {
        Self { inner: Client::new(base_url) }
    }

    /// Start a VM
    pub async fn start_vm(&self, uuid: &Uuid) -> Result<VmActionResponse, Error> {
        self.inner.vm_action()
            .uuid(uuid)
            .action(VmAction::Start)
            .body(serde_json::json!({}))
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Update VM properties
    pub async fn update_vm(
        &self,
        uuid: &Uuid,
        request: &UpdateVmRequest,
    ) -> Result<VmActionResponse, Error> {
        self.inner.vm_action()
            .uuid(uuid)
            .action(VmAction::Update)
            .body(serde_json::to_value(request)?)
            .send()
            .await
            .map(|r| r.into_inner())
    }

    /// Add NICs to a VM
    pub async fn add_nics(
        &self,
        uuid: &Uuid,
        request: &AddNicsRequest,
    ) -> Result<VmActionResponse, Error> {
        self.inner.vm_action()
            .uuid(uuid)
            .action(VmAction::AddNics)
            .body(serde_json::to_value(request)?)
            .send()
            .await
            .map(|r| r.into_inner())
    }
}
```

**Pros:**
- **Single source of truth**: Types defined once in API crate
- **Full type safety at client call sites**: `client.update_vm(uuid, &UpdateVmRequest{...})`
- **Good documentation**: Each action struct fully documented with JsonSchema
- **API compatibility preserved**: Wire format unchanged
- **Clear errors**: Type mismatches caught at compile time in client code
- **Reasonable Progenitor output**: Generic method is fine since we wrap it

**Cons:**
- Hand-written wrapper methods in client crate
- OpenAPI spec shows generic body (less useful for external consumers)
- Additional maintenance when actions change

---

### Option E: Custom OpenAPI Spec with Manual Merging

**Approach:**
1. Generate base spec from Dropshot trait
2. Hand-edit or post-process to add rich `oneOf` body schemas with documentation
3. Use the enhanced spec for client generation

**Pros:**
- OpenAPI spec can be as detailed as desired
- External API consumers get good documentation

**Cons:**
- Spec diverges from code - maintenance burden
- Risk of spec/implementation drift
- Complicates CI/CD (need to validate hand-edits)
- Still doesn't solve the discriminator-in-query-param problem

**Verdict:** Adds complexity without solving the fundamental issue.

---

### Option F: Separate Endpoints with Query Parameter Routing

**Approach:** Define separate trait methods but route them all to the same path in a custom way.

This is not supported by Dropshot - paths must be unique. You cannot have:
```rust
#[endpoint { method = POST, path = "/vms/{uuid}?action=start" }]
async fn start_vm(...);

#[endpoint { method = POST, path = "/vms/{uuid}?action=stop" }]
async fn stop_vm(...);
```

**Verdict:** Not possible with current Dropshot.

---

## Approaches Used by Other Projects

### Kubernetes API

Kubernetes uses subresources for actions: `POST /api/v1/namespaces/{ns}/pods/{name}/exec`. This is RESTful but requires path changes.

### AWS SDK

AWS APIs often use action-based patterns but generate SDKs from separate service definitions (not OpenAPI). Each action is a distinct SDK method with typed inputs.

### GitHub API

Uses RESTful subresources: `PUT /gists/:id/star` rather than `POST /gists/:id?action=star`.

### gRPC/Protobuf

Defines each action as a separate RPC method with typed request/response messages. Code generation produces strongly-typed clients.

**Lesson:** Most well-typed client generation relies on having distinct operations at the schema level, not runtime dispatch.

## Recommendation

**Use Option D: Client Library Depends on API Crate**

This approach provides the best balance of:
1. **Compatibility**: Wire format unchanged
2. **Type safety**: Full compile-time checking at client call sites
3. **Maintainability**: Single source of truth for types
4. **Developer experience**: Typed methods in client library

### Implementation Plan

1. **API Crate (`apis/vmapi-api/`)**
   - Define the trait with `serde_json::Value` body
   - Export all action-specific request/response structs with full documentation
   - Export an `Action` enum for the query parameter

2. **Service Implementation (`services/vmapi-service/`)**
   - Implement the trait
   - Validate and deserialize body based on action
   - Return appropriate errors for invalid action/body combinations

3. **Client Crate (`clients/internal/vmapi-client/`)**
   - Depend on both Progenitor-generated code AND `vmapi-api` crate
   - Add typed wrapper methods for each action
   - Re-export types from `vmapi-api` for convenience

4. **CLI (`cli/vmapi-cli/`)**
   - Use the typed client methods
   - Provide subcommands for each action with proper argument parsing

### Example Directory Structure

```
apis/vmapi-api/
  src/
    lib.rs          # Trait + all types
    actions/
      mod.rs        # Action enum
      start.rs      # StartVmRequest (empty or minimal)
      stop.rs       # StopVmRequest
      update.rs     # UpdateVmRequest with all fields
      add_nics.rs   # AddNicsRequest
      ...

clients/internal/vmapi-client/
  src/
    lib.rs          # Re-exports + wrapper methods
  build.rs          # Progenitor generation

cli/vmapi-cli/
  src/
    main.rs         # Clap app with subcommands
    commands/
      start.rs
      stop.rs
      update.rs
      ...
```

### OpenAPI Documentation Strategy

While the generated OpenAPI spec will show a generic body, you can:

1. Add comprehensive descriptions to the endpoint explaining the action dispatch
2. Include examples for each action type
3. Reference separate documentation for action-specific schemas
4. Consider generating a "documentation-only" OpenAPI spec with full `oneOf` schemas (not used for Progenitor)

## Future Considerations

### OpenAPI 4.0 (Moonwalk)

The [OpenAPI 4.0 specification](https://github.com/OAI/OpenAPI-Specification/discussions/3344) is expected to support "operation signatures" that would allow distinct operations on the same path. This could eventually enable better modeling of action-dispatch patterns.

### Dropshot Enhancements

If this pattern becomes common, consider contributing to Dropshot:
- Support for query-parameter-based routing
- Better `oneOf` body handling with external discriminators

### Alternative: Versioned Migration

For new services or major versions, consider migrating to RESTful subresources:
```
POST /vms/{uuid}/start
POST /vms/{uuid}/stop
POST /vms/{uuid}/nics       (for add_nics)
DELETE /vms/{uuid}/nics/{mac}  (for remove_nics)
```

This provides the best tooling support but requires API version negotiation.

## Conclusion

The action-dispatch pattern is a legacy from RPC-style API design that predates modern OpenAPI tooling. While OpenAPI and tools like Dropshot/Progenitor are optimized for RESTful APIs with distinct paths per operation, we can work within these constraints using Option D.

The key insight is that **type safety doesn't have to come from the OpenAPI spec** - it can come from shared type definitions in the API crate. By making the client library depend on the API crate, we achieve:

- Type safety where it matters most (client call sites)
- API compatibility (wire format unchanged)
- Single source of truth (types defined once)
- Good developer experience (typed methods, good errors)

The trade-off is hand-written wrapper methods in the client, but this is a reasonable price for maintaining compatibility while gaining type safety.

## References

- [OpenAPI Specification v3.1.0](https://spec.openapis.org/oas/v3.1.0)
- [OpenAPI Discriminator Documentation](https://swagger.io/docs/specification/v3_0/data-models/inheritance-and-polymorphism/)
- [Serde Enum Representations](https://serde.rs/enum-representations.html)
- [Dropshot Documentation](https://docs.rs/dropshot/latest/dropshot/)
- [Progenitor GitHub](https://github.com/oxidecomputer/progenitor)
- [Oxide RFD 479: Dropshot API Traits](https://rfd.shared.oxide.computer/rfd/0479)
- [OpenAPI 4.0 Discussion on Single-Path APIs](https://github.com/OAI/OpenAPI-Specification/discussions/3344)
- [REST API Design Best Practices - Moesif](https://www.moesif.com/blog/technical/api-design/REST-API-Design-Best-Practices-for-Parameters-and-Query-String-Usage/)
