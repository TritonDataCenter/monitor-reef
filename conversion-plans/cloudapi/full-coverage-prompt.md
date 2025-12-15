# Agent Prompt: Complete CloudAPI Trait Coverage

## Objective

Bring the CloudAPI Dropshot API trait (`apis/cloudapi-api/`) to 100% coverage of the
functionality implemented in the Node.js CloudAPI service. The goal is that any future
Rust implementation of this trait would be a complete drop-in replacement for the
Node.js service.

## Background

The initial conversion (Phase 1-5) achieved 98.2% endpoint coverage. This task completes
the remaining gaps documented in `conversion-plans/cloudapi/missing-features.md`.

## Source Materials

- **Node.js CloudAPI source**: `/Users/nshalman/Workspace/monitor-reef/target/sdc-cloudapi/`
- **Current Rust API trait**: `apis/cloudapi-api/`
- **Missing features doc**: `conversion-plans/cloudapi/missing-features.md`
- **Validation report**: `conversion-plans/cloudapi/validation.md`
- **Reference API trait**: `apis/bugview-api/` (for patterns)

## Tasks

### Task 1: Add Missing Machine States

**File**: `apis/cloudapi-api/src/types/machine.rs`

Update the `MachineState` enum to include all states that VMAPI can return:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    Running,
    Stopped,
    Stopping,      // ADD: VM is in the process of stopping
    Provisioning,
    Failed,
    Deleted,
    Offline,       // ADD: VM is offline (agent not responding)
    Ready,         // ADD: VM is ready but not started
    Unknown,       // ADD: VM state cannot be determined
}
```

**Verification**: Check `lib/machines.js` function `translate()` for the complete state mapping.

### Task 2: Add WebSocket Endpoints

**File**: `apis/cloudapi-api/src/lib.rs`

Add two WebSocket endpoints. Dropshot supports WebSocket via the `websocket` feature.

#### 2.1 Changefeed Endpoint

**Source**: `lib/changefeed.js`

```rust
/// Stream VM state changes via WebSocket
///
/// Provides real-time notifications of VM state transitions for monitoring
/// tools and dashboards. Requires WebSocket protocol upgrade.
#[endpoint {
    method = GET,
    path = "/{account}/changefeed",
    tags = ["changefeed"],
}]
async fn get_changefeed(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult;
```

#### 2.2 VNC Console Endpoint

**Source**: `lib/endpoints/vnc.js`

```rust
/// Connect to machine VNC console via WebSocket
///
/// Provides browser-based VNC console access to KVM/bhyve virtual machines.
/// Only available for running machines. Requires WebSocket protocol upgrade.
/// Available since API version 8.4.0.
#[endpoint {
    method = GET,
    path = "/{account}/machines/{machine}/vnc",
    tags = ["machines"],
}]
async fn get_machine_vnc(
    rqctx: RequestContext<Self::Context>,
    path: Path<MachinePathParams>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult;
```

**Dependencies**:
- Add `websocket` feature to dropshot dependency in `Cargo.toml`
- Import WebSocket types from dropshot

### Task 3: Add Role Tag Endpoints

**Source**: `lib/resources.js`

The Node.js CloudAPI has generic role tag endpoints using variable path segments that
conflict with Dropshot's routing. To achieve equivalent functionality, add explicit
endpoints for each valid resource type.

#### 3.1 Shared Types

Add to `apis/cloudapi-api/src/types/common.rs` or a new `types/role_tags.rs`:

```rust
/// Request body for replacing role tags on a resource
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoleTagsRequest {
    /// List of role names to assign to the resource
    #[serde(rename = "role-tag", default)]
    pub role_tag: Vec<String>,
}

/// Response after replacing role tags on a resource
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoleTagsResponse {
    /// Resource path name
    pub name: String,
    /// List of role names assigned to the resource
    #[serde(rename = "role-tag")]
    pub role_tag: Vec<String>,
}
```

#### 3.2 Collection-Level Role Tag Endpoints (10 endpoints)

These tag the ability to list/create resources of a given type.

Valid resource names from `lib/resources.js` lines 229-233:
- `machines`, `users`, `roles`, `packages`, `images`, `policies`, `keys`, `datacenters`, `fwrules`, `networks`, `services`

Note: `machines` collection role tags can use the existing account-level endpoint.

Add these endpoints:

```rust
/// Replace role tags on the users collection
#[endpoint {
    method = PUT,
    path = "/{account}/users",
    tags = ["role-tags"],
}]
async fn replace_users_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the roles collection
#[endpoint {
    method = PUT,
    path = "/{account}/roles",
    tags = ["role-tags"],
}]
async fn replace_roles_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the packages collection
#[endpoint {
    method = PUT,
    path = "/{account}/packages",
    tags = ["role-tags"],
}]
async fn replace_packages_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the images collection
#[endpoint {
    method = PUT,
    path = "/{account}/images",
    tags = ["role-tags"],
}]
async fn replace_images_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the policies collection
#[endpoint {
    method = PUT,
    path = "/{account}/policies",
    tags = ["role-tags"],
}]
async fn replace_policies_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the keys collection
#[endpoint {
    method = PUT,
    path = "/{account}/keys",
    tags = ["role-tags"],
}]
async fn replace_keys_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the datacenters collection
#[endpoint {
    method = PUT,
    path = "/{account}/datacenters",
    tags = ["role-tags"],
}]
async fn replace_datacenters_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the firewall rules collection
#[endpoint {
    method = PUT,
    path = "/{account}/fwrules",
    tags = ["role-tags"],
}]
async fn replace_fwrules_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the networks collection
#[endpoint {
    method = PUT,
    path = "/{account}/networks",
    tags = ["role-tags"],
}]
async fn replace_networks_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on the services collection
#[endpoint {
    method = PUT,
    path = "/{account}/services",
    tags = ["role-tags"],
}]
async fn replace_services_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;
```

#### 3.3 Individual Resource Role Tag Endpoints (8 endpoints)

These tag individual resources. Note: `machines` already has `replace_machine_role_tags`.
`datacenters` and `services` are read-only and don't support individual tagging.

```rust
/// Replace role tags on a specific user
#[endpoint {
    method = PUT,
    path = "/{account}/users/{uuid}",
    tags = ["role-tags"],
}]
async fn replace_user_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<UserPathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on a specific role
#[endpoint {
    method = PUT,
    path = "/{account}/roles/{role}",
    tags = ["role-tags"],
}]
async fn replace_role_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<RolePathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on a specific package
#[endpoint {
    method = PUT,
    path = "/{account}/packages/{package}",
    tags = ["role-tags"],
}]
async fn replace_package_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<PackagePathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on a specific image
#[endpoint {
    method = PUT,
    path = "/{account}/images/{dataset}",
    tags = ["role-tags"],
}]
async fn replace_image_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<ImagePathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on a specific policy
#[endpoint {
    method = PUT,
    path = "/{account}/policies/{policy}",
    tags = ["role-tags"],
}]
async fn replace_policy_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<PolicyPathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on a specific SSH key
#[endpoint {
    method = PUT,
    path = "/{account}/keys/{fingerprint}",
    tags = ["role-tags"],
}]
async fn replace_key_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<KeyPathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on a specific firewall rule
#[endpoint {
    method = PUT,
    path = "/{account}/fwrules/{fwrule}",
    tags = ["role-tags"],
}]
async fn replace_fwrule_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<FirewallRulePathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on a specific network
#[endpoint {
    method = PUT,
    path = "/{account}/networks/{network}",
    tags = ["role-tags"],
}]
async fn replace_network_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<NetworkPathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;
```

#### 3.4 User Sub-Resource Role Tags

The endpoint `PUT /{account}/users/{user}/keys` for tagging a user's keys collection
is currently missing. Add:

```rust
/// Replace role tags on a user's keys collection
#[endpoint {
    method = PUT,
    path = "/{account}/users/{uuid}/keys",
    tags = ["role-tags"],
}]
async fn replace_user_keys_collection_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<UserPathParams>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;
```

### Task 4: Update OpenAPI Spec

After adding all endpoints:

```bash
cargo run -p openapi-manager -- generate
```

Verify the spec includes:
- WebSocket endpoints (may need special OpenAPI annotations)
- All 19 new role tag endpoints
- Updated MachineState enum with all values

### Task 5: Update Client Library

The client library (`clients/internal/cloudapi-client/`) auto-generates from the OpenAPI
spec. After regenerating the spec:

```bash
cargo build -p cloudapi-client
```

No manual changes needed unless you want to add typed wrappers for the new endpoints.

### Task 6: Update Documentation

Update `conversion-plans/cloudapi/missing-features.md` to mark completed items.

Update `conversion-plans/cloudapi/validation.md` endpoint count to reflect 100% coverage.

## Verification Steps

1. **Build succeeds**:
   ```bash
   make format package-build PACKAGE=cloudapi-api
   ```

2. **OpenAPI spec generates**:
   ```bash
   cargo run -p openapi-manager -- generate
   ```

3. **Client builds**:
   ```bash
   cargo build -p cloudapi-client
   ```

4. **All checks pass**:
   ```bash
   make check
   ```

5. **Endpoint count**: The OpenAPI spec should have approximately 182 operations:
   - Original: 162 endpoints
   - New role tag endpoints: 19 (10 collection + 8 individual + 1 user keys collection)
   - WebSocket endpoints: 2 (changefeed + VNC)
   - Minus duplicates if any paths conflict with existing PUT endpoints

## Commit Strategy

Make atomic commits for each logical unit:

1. `Add missing machine states to MachineState enum`
2. `Add WebSocket changefeed endpoint`
3. `Add WebSocket VNC endpoint`
4. `Add role tag types (RoleTagsRequest, RoleTagsResponse)`
5. `Add collection-level role tag endpoints`
6. `Add individual resource role tag endpoints`
7. `Update OpenAPI spec for full CloudAPI coverage`
8. `Update documentation to reflect 100% coverage`

## Notes

### WebSocket Considerations

- Dropshot WebSocket support may require specific feature flags
- Check Dropshot documentation for current WebSocket API
- The trait defines the interface; implementation handles actual WebSocket logic

### Route Ordering

Dropshot routes are matched in definition order. Ensure specific routes (like
`PUT /{account}/machines/{machine}`) come before any potential catch-all patterns.

### Backward Compatibility

These additions are purely additive. No existing endpoints are modified, so this
is backward compatible with any existing clients.

### Testing

While the trait itself doesn't include tests, consider:
- Adding doctests showing expected usage
- Creating a stub implementation for compile-time verification
- Integration tests against the Node.js CloudAPI for behavioral verification

## Success Criteria

- [ ] `MachineState` enum has all 9 states
- [ ] WebSocket changefeed endpoint defined
- [ ] WebSocket VNC endpoint defined
- [ ] 10 collection-level role tag endpoints defined
- [ ] 8 individual resource role tag endpoints defined
- [ ] 1 user keys collection role tag endpoint defined
- [ ] OpenAPI spec regenerated with ~182 operations
- [ ] Client library rebuilds successfully
- [ ] `make check` passes
- [ ] Documentation updated

Upon completion, the CloudAPI Dropshot trait will have 100% API surface coverage of
the Node.js CloudAPI, making it suitable as the interface for a drop-in Rust replacement.
