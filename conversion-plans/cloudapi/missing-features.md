# CloudAPI Missing Features and Remaining Work

This document provides a detailed accounting of functionality in the Node.js CloudAPI
that is not yet represented in the Rust Dropshot API trait.

## Summary

**Status: COMPLETE** - All documented missing features have been implemented.

| Category | Status | Notes |
|----------|--------|-------|
| WebSocket Endpoints | ✅ Complete | 2 endpoints added |
| Role Tag Endpoints | ✅ Complete | 21 endpoints added |
| Documentation Redirects | ⏭️ Intentionally Omitted | 3 endpoints (not API functionality) |
| Machine States | ✅ Complete | All 9 states now in enum |

---

## 1. WebSocket Endpoints (2 endpoints) - ✅ COMPLETED

These endpoints use WebSocket protocol upgrades and have been implemented using Dropshot's
`#[channel]` macro.

### 1.1 Changefeed - ✅ IMPLEMENTED

**Source**: `lib/changefeed.js`

| Property | Value |
|----------|-------|
| Method | GET |
| Path | `/{account}/changefeed` |
| Version | 8.0.0+ |
| Protocol | WebSocket |

**Implementation**:
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

### 1.2 VNC Console - ✅ IMPLEMENTED

**Source**: `lib/endpoints/vnc.js`

| Property | Value |
|----------|-------|
| Method | GET |
| Path | `/{account}/machines/{machine}/vnc` |
| Version | 8.4.0+ |
| Protocol | WebSocket |

**Implementation**:
```rust
#[channel {
    protocol = WEBSOCKETS,
    path = "/{account}/machines/{machine}/vnc",
    tags = ["machines"],
}]
async fn get_machine_vnc(
    rqctx: RequestContext<Self::Context>,
    path: Path<MachinePath>,
    upgraded: WebsocketConnection,
) -> WebsocketChannelResult;
```

---

## 2. Role Tag Endpoints (21 endpoints) - ✅ COMPLETED

The Node.js CloudAPI had generic role tag endpoints that used variable path segments.
Since Dropshot cannot support these due to routing ambiguity with literal paths, explicit
endpoints have been added for each valid resource type.

**Source**: `lib/resources.js`

### 2.1 Collection-Level Role Tag Endpoints (10 endpoints) - ✅ IMPLEMENTED

| Endpoint | Status |
|----------|--------|
| `PUT /{account}/users` | ✅ `replace_users_collection_role_tags` |
| `PUT /{account}/roles` | ✅ `replace_roles_collection_role_tags` |
| `PUT /{account}/packages` | ✅ `replace_packages_collection_role_tags` |
| `PUT /{account}/images` | ✅ `replace_images_collection_role_tags` |
| `PUT /{account}/policies` | ✅ `replace_policies_collection_role_tags` |
| `PUT /{account}/keys` | ✅ `replace_keys_collection_role_tags` |
| `PUT /{account}/datacenters` | ✅ `replace_datacenters_collection_role_tags` |
| `PUT /{account}/fwrules` | ✅ `replace_fwrules_collection_role_tags` |
| `PUT /{account}/networks` | ✅ `replace_networks_collection_role_tags` |
| `PUT /{account}/services` | ✅ `replace_services_collection_role_tags` |

### 2.2 Individual Resource Role Tag Endpoints (8 endpoints) - ✅ IMPLEMENTED

Note: `machines` already had `replace_machine_role_tags`. Datacenters and services are
read-only and don't support individual tagging.

| Endpoint | Status |
|----------|--------|
| `PUT /{account}/users/{uuid}` | ✅ `replace_user_role_tags` |
| `PUT /{account}/roles/{role}` | ✅ `replace_role_role_tags` |
| `PUT /{account}/packages/{package}` | ✅ `replace_package_role_tags` |
| `PUT /{account}/images/{dataset}` | ✅ `replace_image_role_tags` |
| `PUT /{account}/policies/{policy}` | ✅ `replace_policy_role_tags` |
| `PUT /{account}/keys/{name}` | ✅ `replace_key_role_tags` |
| `PUT /{account}/fwrules/{id}` | ✅ `replace_fwrule_role_tags` |
| `PUT /{account}/networks/{network}` | ✅ `replace_network_role_tags` |

### 2.3 User Sub-Resource Role Tag Endpoints (2 endpoints) - ✅ IMPLEMENTED

| Endpoint | Status |
|----------|--------|
| `PUT /{account}/users/{uuid}/keys` | ✅ `replace_user_keys_collection_role_tags` |
| `PUT /{account}/users/{uuid}/keys/{name}` | ✅ `replace_user_key_role_tags` |

### 2.4 Shared Types

```rust
/// Request to replace role tags
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplaceRoleTagsRequest {
    #[serde(rename = "role-tag", default)]
    pub role_tag: Vec<String>,
}

/// Response after replacing role tags on a resource
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

## 4. Machine State Enum - ✅ COMPLETED

**Source**: `lib/machines.js` function `translateState()`

The `MachineState` enum now includes all states that can be returned by VMAPI:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MachineState {
    Running,
    Stopped,
    Stopping,      // VM is in the process of stopping
    Provisioning,
    Failed,
    Deleted,
    Offline,       // VM is offline (agent not responding)
    Ready,         // VM is ready but not started
    Unknown,       // VM state cannot be determined
}
```

**State Translation from VMAPI**:
- `configured/incomplete/unavailable/provisioning` → `provisioning`
- `ready` → `ready`
- `running` → `running`
- `halting/stopping/shutting_down` → `stopping`
- `off/down/installed/stopped` → `stopped`
- `unreachable` → `offline`
- `destroyed` → `deleted`
- `failed` → `failed`
- `default` → `unknown`

---

## 5. Implementation Notes

### 5.1 WebSocket Client Dependencies

The `cloudapi-client` crate required additional dependencies for WebSocket support:

```toml
[dependencies]
base64 = "0.22"
rand = "0.9"
```

These are used by Progenitor-generated WebSocket client code.

### 5.2 Endpoint Count

The OpenAPI spec now contains **183 operations**:
- Original: 162 endpoints
- New role tag endpoints: 19 (10 collection + 8 individual + 1 user keys collection)
- WebSocket endpoints: 2 (changefeed + VNC)

---

## 6. References

- Original Node.js source: `/Users/nshalman/Workspace/monitor-reef/target/sdc-cloudapi/`
- Rust API trait: `apis/cloudapi-api/`
- Validation report: `conversion-plans/cloudapi/validation.md`
- Conversion plan: `conversion-plans/cloudapi/plan.md`

---

*Updated: 2025-12-15*
*All documented missing features have been implemented.*
