# Missing Migration Endpoints in CloudAPI Trait

**Date:** 2025-12-16
**Status:** ✅ Implemented
**Priority:** High (blocks full migration CLI functionality)

## Discovery

While implementing the `triton instance migration` CLI commands as part of the high-priority work plan, I discovered that the `cloudapi-api` trait is missing several migration-related endpoints.

### Current State

The `cloudapi-api` trait (in `apis/cloudapi-api/src/lib.rs`) currently defines these migration endpoints:

| Endpoint | Method | Path | Function |
|----------|--------|------|----------|
| List migrations | GET | `/{account}/migrations` | `list_migrations` |
| Get migration | GET | `/{account}/migrations/{machine}` | `get_migration` |
| Estimate migration | GET | `/{account}/machines/{machine}/migrate` | `migrate_machine_estimate` |
| Start migration | POST | `/{account}/machines/{machine}/migrate` | `migrate` |

### Missing Endpoints

Based on the Triton CloudAPI documentation and the Node.js implementation, the following endpoints are missing:

#### 1. Finalize Migration

Switches the instance to the new server after migration sync is complete.

```
POST /{account}/migrations/{machine}?action=switch
```

Or alternatively (depending on CloudAPI version):
```
POST /{account}/machines/{machine}/migrate?action=switch
```

#### 2. Abort Migration

Cancels a migration in progress and cleans up the migration state.

```
POST /{account}/migrations/{machine}?action=abort
```

Or alternatively:
```
DELETE /{account}/migrations/{machine}
```

#### 3. Pause Migration (optional)

Pauses an in-progress migration.

```
POST /{account}/migrations/{machine}?action=pause
```

#### 4. Watch Migration (optional)

WebSocket endpoint for real-time migration progress updates.

```
GET /{account}/migrations/{machine}/watch (WebSocket upgrade)
```

## Impact

Without the finalize and abort endpoints:
- Users cannot complete migrations (switch to new server)
- Users cannot cancel migrations if something goes wrong
- The migration workflow is incomplete

## Recommended Actions

### Step 1: Verify Endpoints Against Node.js CloudAPI

Review the Node.js CloudAPI source code to confirm the exact endpoint signatures:

```bash
# In the node-triton or cloudapi repository
grep -r "migration" lib/endpoints/ --include="*.js"
```

Key files to check:
- `lib/endpoints/migrations.js`
- `lib/endpoints/machines.js` (migrate action)

### Step 2: Add Missing Types

Add any missing request/response types to `apis/cloudapi-api/src/types/misc.rs`:

```rust
/// Request to control migration (switch/abort/pause)
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationActionRequest {
    /// Action to perform: "switch", "abort", or "pause"
    pub action: MigrationAction,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MigrationAction {
    Switch,
    Abort,
    Pause,
}
```

### Step 3: Add Endpoints to Trait

Add the missing endpoints to `apis/cloudapi-api/src/lib.rs`:

```rust
/// Perform migration action (switch/abort/pause)
#[endpoint {
    method = POST,
    path = "/{account}/migrations/{machine}",
    tags = ["migrations"],
}]
async fn migration_action(
    rqctx: RequestContext<Self::Context>,
    path: Path<MachinePath>,
    query: Query<MigrationActionQuery>,
) -> Result<HttpResponseOk<Migration>, HttpError>;
```

Or if separate endpoints are preferred:

```rust
/// Finalize migration (switch to new server)
#[endpoint {
    method = POST,
    path = "/{account}/migrations/{machine}/switch",
    tags = ["migrations"],
}]
async fn finalize_migration(
    rqctx: RequestContext<Self::Context>,
    path: Path<MachinePath>,
) -> Result<HttpResponseOk<Migration>, HttpError>;

/// Abort migration
#[endpoint {
    method = DELETE,
    path = "/{account}/migrations/{machine}",
    tags = ["migrations"],
}]
async fn abort_migration(
    rqctx: RequestContext<Self::Context>,
    path: Path<MachinePath>,
) -> Result<HttpResponseDeleted, HttpError>;
```

### Step 4: Regenerate OpenAPI Spec

```bash
make openapi-generate
git add openapi-specs/generated/
```

### Step 5: Update CLI Implementation

Once the endpoints are added, update `cli/triton-cli/src/commands/instance/migration.rs` to use the correct method names.

## Workaround

Until the endpoints are added, the CLI can implement a partial migration workflow:
- `triton instance migration get` - View migration status ✓
- `triton instance migration estimate` - Estimate migration ✓
- `triton instance migration start` - Start migration ✓
- `triton instance migration wait` - Poll until complete ✓
- `triton instance migration finalize` - **BLOCKED**
- `triton instance migration abort` - **BLOCKED**

## Related Files

- `apis/cloudapi-api/src/lib.rs` - API trait definition
- `apis/cloudapi-api/src/types/misc.rs` - Migration types
- `cli/triton-cli/src/commands/instance/migration.rs` - CLI implementation (in progress)
- `conversion-plans/triton/plan-high-priority-2025-12-16.md` - Original work plan

## Implementation Summary (2025-12-16)

The analysis revealed that the existing API design was mostly correct - all migration actions go through a single POST endpoint with the action specified in the request body. The following changes were made:

### Types Added (`apis/cloudapi-api/src/types/misc.rs`)

1. **`MigrationAction` enum** - Typed enum for all valid migration actions:
   - `begin`, `sync`, `switch`, `automatic`, `abort`, `pause`, `finalize`

2. **`MigrationProgressEntry` struct** - For detailed progress history tracking

3. **Updated `Migration` struct** - Added:
   - `finished_timestamp` field
   - `progress_history` field for detailed tracking
   - Better documentation

4. **Updated `MigrateRequest` struct** - Changed `action` from `String` to `MigrationAction` enum for type safety

### Endpoints Added (`apis/cloudapi-api/src/lib.rs`)

1. **`watch_migration`** - WebSocket endpoint at `/{account}/migrations/{machine}/watch` for real-time progress streaming

### Documentation Improvements

- Added comprehensive documentation for the `migrate` endpoint explaining all action types
- Added documentation for existing migration endpoints

### Key Finding

The Node.js CloudAPI uses a **single POST endpoint** (`POST /{account}/machines/{machine}/migrate`) with the `action` field in the request body to handle all migration operations. This design was already present in the Rust trait, but now with:
- Type-safe `MigrationAction` enum instead of free-form string
- WebSocket endpoint for progress watching
- Better documentation

### Files Changed

- `apis/cloudapi-api/src/types/misc.rs` - Added types
- `apis/cloudapi-api/src/lib.rs` - Added WebSocket endpoint, improved docs
- `openapi-specs/generated/cloudapi-api.json` - Regenerated

## References

- Triton CloudAPI Migration Documentation
- Node.js CloudAPI source: `lib/endpoints/migrations.js`
- VMAPI Migration API (upstream)
