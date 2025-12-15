# CloudAPI Missing Features and Remaining Work

This document provides a detailed accounting of functionality in the Node.js CloudAPI
that is not yet represented in the Rust Dropshot API trait.

## Summary

| Category | Missing Items | Priority |
|----------|---------------|----------|
| WebSocket Endpoints | 2 endpoints | High |
| Role Tag Endpoints | 21 endpoints | Medium |
| Documentation Redirects | 3 endpoints | Low (intentional) |
| Machine States | 4 enum values | Low |
| Total | 30 items | |

---

## 1. WebSocket Endpoints (2 endpoints)

These endpoints use WebSocket protocol upgrades and require special handling in Dropshot.

### 1.1 Changefeed

**Source**: `lib/changefeed.js`

| Property | Value |
|----------|-------|
| Method | GET |
| Path | `/:account/changefeed` |
| Version | 8.0.0+ |
| Protocol | WebSocket |

**Purpose**: Streams real-time VM state changes to monitoring tools and dashboards.

**Node.js Implementation Details**:
- Uses `watershed` library for WebSocket handling
- Connects to internal VMAPI changefeed
- Streams JSON messages for VM state transitions
- Supports filtering by VM UUID

**Dropshot Implementation Path**:
```rust
#[endpoint {
    method = GET,
    path = "/{account}/changefeed",
    tags = ["changefeed"],
}]
async fn get_changefeed(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    websocket: WebsocketConnection,
) -> WebsocketChannelResult;
```

**Dependencies**:
- Dropshot `websocket` feature flag
- Internal VMAPI changefeed connection
- State management for active connections

### 1.2 VNC Console

**Source**: `lib/endpoints/vnc.js`

| Property | Value |
|----------|-------|
| Method | GET |
| Path | `/:account/machines/:machine/vnc` |
| Version | 8.4.0+ |
| Protocol | WebSocket |

**Purpose**: Provides browser-based VNC console access to virtual machines.

**Node.js Implementation Details**:
- Uses `watershed` library for WebSocket handling
- Proxies VNC connection from compute node to client
- Only available for running KVM/bhyve machines
- Requires compute node connectivity

**Dropshot Implementation Path**:
```rust
#[endpoint {
    method = GET,
    path = "/{account}/machines/{machine}/vnc",
    tags = ["machines"],
}]
async fn get_machine_vnc(
    rqctx: RequestContext<Self::Context>,
    path: Path<MachinePathParams>,
    websocket: WebsocketConnection,
) -> WebsocketChannelResult;
```

**Dependencies**:
- Dropshot `websocket` feature flag
- Compute node VNC proxy logic
- Machine state validation (must be running KVM/bhyve)

---

## 2. Role Tag Endpoints (21 endpoints)

The Node.js CloudAPI has generic role tag endpoints that use variable path segments.
Dropshot cannot support these due to routing ambiguity with literal paths.

**Source**: `lib/resources.js`

### 2.1 Valid Resource Names

From `lib/resources.js` lines 229-233:
```javascript
var validResources = [
    'machines', 'users', 'roles', 'packages',
    'images', 'policies', 'keys', 'datacenters',
    'fwrules', 'networks', 'services'
];
```

### 2.2 Generic Endpoints (Cannot Implement Directly)

These 3 generic endpoints cannot be implemented in Dropshot:

| Node.js Route | Handler | Issue |
|---------------|---------|-------|
| `PUT /:account/:resource_name` | ReplaceResourcesRoleTags | `{resource_name}` conflicts with literal paths |
| `PUT /:account/:resource_name/:resource_id` | ReplaceResourceRoleTags | `{resource_name}` conflicts with literal paths |
| `PUT /:account/users/:user/:resource_name` | ReplaceUserKeysResourcesRoleTags | `{resource_name}` conflicts with literal paths |

### 2.3 Currently Implemented

| Endpoint | Handler | Status |
|----------|---------|--------|
| `PUT /{account}` | ReplaceAccountRoleTags | ✅ Implemented |
| `PUT /{account}/machines/{machine}` | ReplaceMachineRoleTags | ✅ Implemented |
| `PUT /{account}/users/{user}/keys/{key}` | ReplaceUserKeysResourceRoleTags | ✅ Implemented |

### 2.4 Missing Specific Endpoints (18 endpoints needed)

To achieve full role tag coverage, explicit endpoints are needed for each resource type:

#### Collection-Level Role Tags (10 endpoints)

| Endpoint | Purpose |
|----------|---------|
| `PUT /{account}/users` | Tag the users collection |
| `PUT /{account}/roles` | Tag the roles collection |
| `PUT /{account}/packages` | Tag the packages collection |
| `PUT /{account}/images` | Tag the images collection |
| `PUT /{account}/policies` | Tag the policies collection |
| `PUT /{account}/keys` | Tag the keys collection |
| `PUT /{account}/datacenters` | Tag the datacenters collection |
| `PUT /{account}/fwrules` | Tag the firewall rules collection |
| `PUT /{account}/networks` | Tag the networks collection |
| `PUT /{account}/services` | Tag the services collection |

#### Individual Resource Role Tags (8 endpoints)

Note: `machines` already has a specific endpoint.

| Endpoint | Purpose |
|----------|---------|
| `PUT /{account}/users/{uuid}` | Tag individual user |
| `PUT /{account}/roles/{uuid}` | Tag individual role |
| `PUT /{account}/packages/{uuid}` | Tag individual package |
| `PUT /{account}/images/{uuid}` | Tag individual image |
| `PUT /{account}/policies/{uuid}` | Tag individual policy |
| `PUT /{account}/keys/{fingerprint}` | Tag individual SSH key |
| `PUT /{account}/fwrules/{uuid}` | Tag individual firewall rule |
| `PUT /{account}/networks/{uuid}` | Tag individual network |

Note: `datacenters` and `services` are read-only and don't need individual role tag endpoints.

### 2.5 Implementation Approach

Each endpoint follows the same pattern:

```rust
/// Replace role tags on the images collection
#[endpoint {
    method = PUT,
    path = "/{account}/images",
    tags = ["role-tags"],
}]
async fn replace_images_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<AccountPath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;

/// Replace role tags on an individual image
#[endpoint {
    method = PUT,
    path = "/{account}/images/{image}",
    tags = ["role-tags"],
}]
async fn replace_image_role_tags(
    rqctx: RequestContext<Self::Context>,
    path: Path<ImagePath>,
    body: TypedBody<RoleTagsRequest>,
) -> Result<HttpResponseOk<RoleTagsResponse>, HttpError>;
```

### 2.6 Shared Types

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoleTagsRequest {
    #[serde(rename = "role-tag")]
    pub role_tag: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoleTagsResponse {
    pub name: String,
    #[serde(rename = "role-tag")]
    pub role_tag: Vec<String>,
}
```

---

## 3. Documentation Redirect Endpoints (3 endpoints - Intentionally Omitted)

**Source**: `lib/docs.js`

These endpoints redirect to external documentation and are not part of the API functionality.

| Endpoint | Redirect Target | Reason for Omission |
|----------|-----------------|---------------------|
| `GET /` | `http://apidocs.tritondatacenter.com/` | Not API functionality |
| `GET /docs/*` | `http://apidocs.tritondatacenter.com/cloudapi/` | Not API functionality |
| `GET /favicon.ico` | `http://apidocs.tritondatacenter.com/favicon.ico` | Static asset |

**Recommendation**: Handle these at the reverse proxy or HTTP server level, not in the API trait.

---

## 4. Machine State Enum (4 missing values)

**Source**: `lib/machines.js` function `translate()`

The current `MachineState` enum is missing some states that can be returned by VMAPI.

### 4.1 Currently Implemented States

```rust
pub enum MachineState {
    Running,
    Stopped,
    Provisioning,
    Failed,
    Deleted,
}
```

### 4.2 Missing States

| State | When Used |
|-------|-----------|
| `stopping` | VM is in the process of stopping |
| `offline` | VM is offline (agent not responding) |
| `ready` | VM is ready but not started |
| `unknown` | VM state cannot be determined |

### 4.3 Updated Enum

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    Running,
    Stopped,
    Stopping,
    Provisioning,
    Failed,
    Deleted,
    Offline,
    Ready,
    Unknown,
}
```

---

## 5. Additional Behavioral Gaps

These are implementation concerns rather than API surface gaps, but are worth noting.

### 5.1 API Versioning

**Node.js**: Uses `restify-clients` versioning with `Accept-Version` header. Many endpoints
have version constraints (e.g., `version: ['7.2.0', '7.3.0', '8.0.0', '9.0.0']`).

**Rust API**: No versioning mechanism. All endpoints assume latest version (9.20.0).

**Options**:
1. Add version checks in implementation middleware
2. Create version-specific trait variants
3. Document minimum version requirements per endpoint

### 5.2 Async Job Responses

**Node.js**: Many mutating operations return immediately with a job ID. The client must
poll for completion.

**Rust API**: Job responses not explicitly modeled. The implementation must handle:
- Returning job location headers
- Supporting `?sync=true` query parameter for synchronous operations
- Job polling endpoints

**Affected Operations**:
- Machine create/delete/start/stop/reboot/resize
- Snapshot create/delete
- Image create/export/clone/import
- Volume create/delete

### 5.3 Content-Type Handling

**Node.js**: Several endpoints accept multiple content types:
- `application/json`
- `application/x-www-form-urlencoded`
- `multipart/form-data`

**Rust API**: Only JSON modeled. Form data handling would need additional work.

**Affected Endpoints**:
- Key creation (can accept raw key data)
- Policy creation
- User creation

### 5.4 Response Headers

**Node.js**: Sets various response headers that aren't modeled:
- `x-query-limit` / `x-resource-count` for pagination
- `role-tag` header on responses
- `x-joyent-software-version` header

**Rust API**: These would be set by the implementation, not the trait.

---

## 6. Implementation Checklist

### High Priority (Production Blockers)

- [ ] Add WebSocket changefeed endpoint
- [ ] Add WebSocket VNC endpoint
- [ ] Add missing machine states to enum
- [ ] Integration test against live Node.js CloudAPI

### Medium Priority (Feature Completeness)

- [ ] Add 10 collection-level role tag endpoints
- [ ] Add 8 individual resource role tag endpoints
- [ ] Model async job responses
- [ ] Add `?sync=true` query parameter support

### Low Priority (Polish)

- [ ] Document API versioning strategy
- [ ] Add form data content-type support
- [ ] Document response header expectations
- [ ] Add OpenAPI examples from real responses

---

## 7. Estimated Work

| Task | Effort | Dependencies |
|------|--------|--------------|
| WebSocket endpoints | 2-3 days | Dropshot websocket feature, internal service connections |
| Role tag endpoints (18) | 1-2 days | None (straightforward addition) |
| Machine state enum | 30 minutes | None |
| Job response modeling | 1 day | Design decision on response types |
| Integration tests | 2-3 days | Access to live CloudAPI instance |

**Total estimated effort**: 1-2 weeks for full feature parity

---

## 8. References

- Original Node.js source: `/Users/nshalman/Workspace/monitor-reef/target/sdc-cloudapi/`
- Rust API trait: `apis/cloudapi-api/`
- Validation report: `conversion-plans/cloudapi/validation.md`
- Conversion plan: `conversion-plans/cloudapi/plan.md`
