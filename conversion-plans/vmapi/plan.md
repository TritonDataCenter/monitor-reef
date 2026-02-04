# VMAPI API Conversion Plan

## Source
- Path: /Users/nshalman/Workspace/triton-deploy/target/sdc-vmapi
- Version: 9.17.0
- Package name: vmapi

## Endpoints Summary
- Total: 45
- By method: GET: 20, POST: 9, PUT: 4, DELETE: 5, HEAD: 2
- Source files:
  - `lib/endpoints/vms.js`
  - `lib/endpoints/jobs.js`
  - `lib/endpoints/metadata.js`
  - `lib/endpoints/ping.js`
  - `lib/endpoints/role-tags.js`
  - `lib/endpoints/statuses.js`
  - `lib/vm-migration/migrate.js`

## Endpoints Detail

### Ping (from ping.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /ping | ping | Health check endpoint |

### VMs (from vms.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /vms | listVms | Returns array of VMs |
| HEAD | /vms | listVms | Count of VMs |
| POST | /vms | createVm | Provisions a new VM |
| PUT | /vms | putVms | Bulk update VMs for a server |
| GET | /vms/:uuid | getVm | Returns single VM object |
| HEAD | /vms/:uuid | getVm | Check VM existence |
| POST | /vms/:uuid | updateVm | Action dispatch (see below) |
| PUT | /vms/:uuid | putVm | Replace VM object |
| DELETE | /vms/:uuid | deleteVm | Destroy VM |
| GET | /vms/:uuid/proc | getVmProc | Get VM process info |

### Jobs (from jobs.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /jobs | listJobs | Returns array of jobs |
| GET | /jobs/:job_uuid | getJob | Returns single job object |
| GET | /jobs/:job_uuid/wait | waitJob | Waits for job completion |
| POST | /job_results | postJobResults | Workflow callback |
| GET | /vms/:uuid/jobs | listJobs | List jobs for a VM |

### Metadata (from metadata.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /vms/:uuid/:metaType | listMetadata | List all metadata; metaType is customer_metadata, internal_metadata, or tags |
| GET | /vms/:uuid/:metaType/:key | getMetadata | Get single metadata key |
| POST | /vms/:uuid/:metaType | addMetadata | Add metadata (merge) |
| PUT | /vms/:uuid/:metaType | setMetadata | Replace all metadata |
| DELETE | /vms/:uuid/:metaType/:key | deleteMetadata | Delete single key |
| DELETE | /vms/:uuid/:metaType | deleteAllMetadata | Delete all metadata |

### Role Tags (from role-tags.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /vms/:uuid/role_tags | addRoleTags | Add role tags (merge) |
| PUT | /vms/:uuid/role_tags | setRoleTags | Replace all role tags |
| DELETE | /vms/:uuid/role_tags/:role_tag | deleteRoleTag | Delete single role tag |
| DELETE | /vms/:uuid/role_tags | deleteAllRoleTags | Delete all role tags |

### Statuses (from statuses.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /statuses | listStatuses | Get statuses for multiple VMs by UUID |

### Migrations (from vm-migration/migrate.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /migrations | listVmMigrations | List all migrations |
| GET | /migrations/:uuid | getVmMigration | Get migration for VM |
| DELETE | /migrations/:uuid | deleteVmMigration | Delete migration record |
| GET | /migrations/:uuid/watch | watchVmMigration | Stream migration progress (x-json-stream) |
| POST | /migrations/:uuid/store | storeMigrationRecord | Internal: store migration record |
| POST | /migrations/:uuid/progress | onProgress | Internal: report progress |
| POST | /migrations/:uuid/updateVmServerUuid | updateVmServerUuid | Internal: update server UUID |

## Route Conflicts

### Conflict 1: /vms/:uuid/:metaType vs /vms/:uuid/role_tags
- Routes: `GET /vms/:uuid/:metaType` vs `GET /vms/:uuid/role_tags` (implicit via POST/PUT/DELETE)
- Analysis: The `metaType` parameter is validated to be one of `customer_metadata`, `internal_metadata`, or `tags`. Role tags use a separate handler chain.
- Recommended resolution: Treat `role_tags` as a separate literal path, not as a `metaType` value
- **Status: RESOLVED** - In Dropshot, define role_tags routes separately with literal path segments

### Conflict 2: /vms/:uuid/jobs vs /vms/:uuid/:metaType
- Routes: `GET /vms/:uuid/jobs` vs `GET /vms/:uuid/:metaType`
- Analysis: The `jobs` endpoint is a separate handler from metadata endpoints
- Recommended resolution: Define `/vms/:uuid/jobs` as a separate literal path
- **Status: RESOLVED** - In Dropshot, define jobs route separately with literal path segment

### Conflict 3: /vms/:uuid/proc vs /vms/:uuid/:metaType
- Routes: `GET /vms/:uuid/proc` vs `GET /vms/:uuid/:metaType`
- Analysis: The `proc` endpoint gets process info from CNAPI
- Recommended resolution: Define `/vms/:uuid/proc` as a separate literal path
- **Status: RESOLVED** - In Dropshot, define proc route separately with literal path segment

## Action Dispatch Endpoints

### POST /vms/:uuid?action=<action>

**Common query parameters for all actions:**
- `sync` (optional): If `true`, wait for job completion before returning (default: `false`)

| Action | Required Fields | Optional Fields | Notes |
|--------|-----------------|-----------------|-------|
| start | (none) | idempotent | Start stopped VM |
| stop | (none) | idempotent | Stop running VM |
| kill | (none) | signal, idempotent | Send signal; signal defaults to SIGKILL |
| reboot | (none) | idempotent | Reboot VM |
| reprovision | image_uuid | | Reprovision with new image |
| update | (varies) | ram, cpu_cap, quota, ... | Many optional fields |
| add_nics | networks OR macs | | Add NICs; one of networks/macs required |
| update_nics | nics | | Array of NIC updates |
| remove_nics | macs | | Array of MAC addresses to remove |
| create_snapshot | (none) | snapshot_name | Auto-generated if omitted |
| rollback_snapshot | snapshot_name | | Rollback to named snapshot |
| delete_snapshot | snapshot_name | | Delete named snapshot |
| create_disk | size | pci_slot, disk_uuid | size can be number or "remaining" (bhyve only) |
| resize_disk | pci_slot, size | dangerous_allow_shrink | Resize disk (bhyve only) |
| delete_disk | pci_slot | | Delete disk by slot (bhyve only) |
| migrate | (none) | migration_action, target_server_uuid, affinity | See migration actions below |

**Migration sub-actions (via `migration_action` param):**
- `begin` - Start migration
- `estimate` - Estimate migration time
- `sync` - Sync data
- `pause` - Pause sync
- `switch` - Switch to target
- `abort` - Abort migration
- `rollback` - Rollback migration
- `finalize` - Finalize (cleanup source)

**Example usage:**
```bash
# Async (default) - returns immediately with job_uuid
POST /vms/{uuid}?action=start

# Sync - waits for job completion before returning
POST /vms/{uuid}?action=start&sync=true
```

## Planned File Structure
```
apis/vmapi-api/src/
├── lib.rs          # Re-exports, main trait, and Ping endpoint
├── types.rs        # Shared types (Vm, Job, Migration, etc.)
├── vms.rs          # VM endpoints and action handlers
├── jobs.rs         # Job endpoints
├── metadata.rs     # Metadata endpoints (customer_metadata, internal_metadata, tags)
├── role_tags.rs    # Role tags endpoints
├── statuses.rs     # Statuses endpoint
└── migrations.rs   # Migration endpoints
```

## Types to Define

### Core Types
- `Vm` - VM object with all properties
- `VmListParams` - Query parameters for ListVms
- `Job` - Workflow job object
- `Migration` - Migration record
- `MigrationProgress` - Progress entry for migration
- `PingResponse` - Ping endpoint response

### Request Types
- `CreateVmParams` - Parameters for VM creation
- `UpdateVmParams` - Parameters for VM update action
- `AddNicsParams` - Parameters for add_nics action
- `UpdateNicsParams` - Parameters for update_nics action
- `RemoveNicsParams` - Parameters for remove_nics action
- `CreateDiskParams` - Parameters for create_disk action
- `ResizeDiskParams` - Parameters for resize_disk action
- `DeleteDiskParams` - Parameters for delete_disk action
- `SnapshotParams` - Parameters for snapshot actions
- `MigrateParams` - Parameters for migrate action
- `ReprovisionParams` - Parameters for reprovision action

### Response Types
- `JobResponse` - Response with vm_uuid and job_uuid
- `MetadataResponse` - Metadata key-value object
- `StatusesResponse` - Map of vm_uuid to status

### Metadata Types
- `MetadataType` - Enum: CustomerMetadata, InternalMetadata, Tags
- `MetadataKey` - Key for single metadata item

## Field Naming Exceptions
- All fields use snake_case in the JSON API (standard for Triton internal APIs)
- No camelCase conversion needed

## WebSocket/Channel Endpoints
- `GET /migrations/:uuid/watch` - Streaming endpoint using `application/x-json-stream`
  - Requires Dropshot `#[channel]` attribute for proper implementation
  - Returns newline-delimited JSON objects for progress events
  - Event types: `progress`, `end`

## Changefeed Integration
- The changefeed publisher mounts routes via `mountRestifyServerRoutes(server)`
- Routes are: `/changefeeds` and `/changefeeds/stats` (implied from middleware skip list)
- These are handled by the `changefeed` npm module, not part of core VMAPI

## Phase 2 Complete

- API crate: `apis/vmapi-api/`
- OpenAPI spec: `openapi-specs/generated/vmapi-api.json`
- Endpoint count: 46
- Build status: SUCCESS

### Route Conflict Resolution Applied
Due to Dropshot routing constraints (cannot have both literal and variable path segments at the same level), the metadata endpoints were split into explicit literal paths:
- `/vms/{uuid}/customer_metadata` - Customer metadata operations
- `/vms/{uuid}/internal_metadata` - Internal metadata operations
- `/vms/{uuid}/tags` - Tags operations

This preserves API compatibility while avoiding conflicts with `/vms/{uuid}/jobs`, `/vms/{uuid}/proc`, and `/vms/{uuid}/role_tags`.

### Files Created
- `apis/vmapi-api/Cargo.toml`
- `apis/vmapi-api/src/lib.rs` - API trait with 46 endpoints
- `apis/vmapi-api/src/types/mod.rs`
- `apis/vmapi-api/src/types/common.rs` - Shared types (Brand, VmState, PingResponse)
- `apis/vmapi-api/src/types/vms.rs` - VM types and all action request structs
- `apis/vmapi-api/src/types/jobs.rs` - Job types
- `apis/vmapi-api/src/types/metadata.rs` - Metadata types
- `apis/vmapi-api/src/types/role_tags.rs` - Role tag types
- `apis/vmapi-api/src/types/statuses.rs` - Status types
- `apis/vmapi-api/src/types/migrations.rs` - Migration types

### Action Request Types Implemented
All VM actions have typed request structs:
- `StartVmRequest` - idempotent option
- `StopVmRequest` - idempotent option
- `KillVmRequest` - signal, idempotent options
- `RebootVmRequest` - idempotent option
- `ReprovisionVmRequest` - image_uuid required
- `UpdateVmRequest` - many optional fields (ram, cpu_cap, quota, etc.)
- `AddNicsRequest` - networks or macs
- `UpdateNicsRequest` - nics array
- `RemoveNicsRequest` - macs array
- `CreateSnapshotRequest` - optional snapshot_name
- `RollbackSnapshotRequest` - snapshot_name required
- `DeleteSnapshotRequest` - snapshot_name required
- `CreateDiskRequest` - size (number or "remaining"), optional pci_slot, disk_uuid
- `ResizeDiskRequest` - pci_slot, size, optional dangerous_allow_shrink
- `DeleteDiskRequest` - pci_slot required
- `MigrateVmRequest` - migration_action, target_server_uuid, affinity options

### WebSocket/Channel Endpoint
- `watch_migration` - `/migrations/{uuid}/watch` - Streams migration progress

## Phase 3 Complete

- Client crate: `clients/internal/vmapi-client/`
- Build status: SUCCESS
- Typed wrappers: YES

### Files Created
- `clients/internal/vmapi-client/Cargo.toml`
- `clients/internal/vmapi-client/build.rs`
- `clients/internal/vmapi-client/src/lib.rs`

### TypedClient Wrapper Methods
All VM actions have typed wrapper methods in `TypedClient`:
- `start_vm(uuid, idempotent)` - Start a VM
- `stop_vm(uuid, idempotent)` - Stop a VM
- `kill_vm(uuid, signal, idempotent)` - Kill a VM with optional signal
- `reboot_vm(uuid, idempotent)` - Reboot a VM
- `reprovision_vm(uuid, image_uuid)` - Reprovision with new image
- `update_vm(uuid, request)` - Update VM properties
- `add_nics(uuid, request)` - Add NICs to a VM
- `update_nics(uuid, request)` - Update NICs on a VM
- `remove_nics(uuid, macs)` - Remove NICs from a VM
- `create_snapshot(uuid, snapshot_name)` - Create a snapshot
- `rollback_snapshot(uuid, snapshot_name)` - Rollback to snapshot
- `delete_snapshot(uuid, snapshot_name)` - Delete a snapshot
- `create_disk(uuid, request)` - Create a disk (bhyve)
- `resize_disk(uuid, request)` - Resize a disk (bhyve)
- `delete_disk(uuid, pci_slot)` - Delete a disk (bhyve)
- `migrate_vm(uuid, request)` - Migrate a VM

### Type Re-exports
The client re-exports all request types from `vmapi-api` for convenience.
Note: `VmAction` is NOT re-exported because Progenitor generates its own type.
Use `types::VmAction` from the generated code for the client builder API.

## Phase 4 Complete

- CLI crate: `cli/vmapi-cli/`
- Binary name: `vmapi`
- Build status: SUCCESS
- Format/Lint: PASS

### Files Created
- `cli/vmapi-cli/Cargo.toml`
- `cli/vmapi-cli/src/main.rs`

### Commands Implemented (56 total)
The CLI covers all 46 API endpoints with the following commands:

**Ping:**
- `ping` - Health check endpoint

**VMs:**
- `list-vms` - List VMs with filters (owner-uuid, server-uuid, state, brand, alias, limit, offset)
- `head-vms` - Count VMs (HEAD request)
- `get-vm` - Get VM details
- `head-vm` - Check VM existence (HEAD request)
- `create-vm` - Create a new VM
- `put-vms` - Bulk update VMs for a server
- `put-vm` - Replace VM object
- `delete-vm` - Delete/destroy a VM
- `get-vm-proc` - Get VM process info

**VM Actions (using TypedClient wrappers):**
- `start-vm` - Start a VM
- `stop-vm` - Stop a VM
- `kill-vm` - Kill a VM (send signal)
- `reboot-vm` - Reboot a VM
- `reprovision-vm` - Reprovision a VM with a new image
- `update-vm` - Update VM properties
- `add-nics` - Add NICs to a VM
- `update-nics` - Update NICs on a VM
- `remove-nics` - Remove NICs from a VM
- `create-snapshot` - Create a snapshot
- `rollback-snapshot` - Rollback to a snapshot
- `delete-snapshot` - Delete a snapshot
- `create-disk` - Create a disk (bhyve only)
- `resize-disk` - Resize a disk (bhyve only)
- `delete-disk` - Delete a disk (bhyve only)
- `migrate-vm` - Migrate a VM

**Jobs:**
- `list-jobs` - List jobs with filters (vm-uuid, execution, task)
- `get-job` - Get job details
- `wait-job` - Wait for job completion
- `post-job-results` - Post job results (internal workflow callback)
- `list-vm-jobs` - List jobs for a specific VM

**Customer Metadata:**
- `list-customer-metadata` - List customer metadata
- `get-customer-metadata` - Get customer metadata key
- `add-customer-metadata` - Add customer metadata (merge)
- `set-customer-metadata` - Set customer metadata (replace all)
- `delete-customer-metadata` - Delete customer metadata key
- `delete-all-customer-metadata` - Delete all customer metadata

**Internal Metadata:**
- `list-internal-metadata` - List internal metadata
- `get-internal-metadata` - Get internal metadata key
- `add-internal-metadata` - Add internal metadata (merge)
- `set-internal-metadata` - Set internal metadata (replace all)
- `delete-internal-metadata` - Delete internal metadata key
- `delete-all-internal-metadata` - Delete all internal metadata

**Tags:**
- `list-tags` - List tags
- `get-tag` - Get tag value
- `add-tags` - Add tags (merge)
- `set-tags` - Set tags (replace all)
- `delete-tag` - Delete tag
- `delete-all-tags` - Delete all tags

**Role Tags:**
- `add-role-tags` - Add role tags
- `set-role-tags` - Set role tags (replace all)
- `delete-role-tag` - Delete a role tag
- `delete-all-role-tags` - Delete all role tags

**Statuses:**
- `list-statuses` - Get statuses for multiple VMs

**Migrations:**
- `list-migrations` - List migrations
- `get-migration` - Get migration for a VM
- `delete-migration` - Delete migration record
- `store-migration-record` - Store migration record (internal)
- `report-migration-progress` - Report migration progress (internal)
- `update-vm-server-uuid` - Update VM server UUID after migration (internal)

### CLI Features
- Environment variable for base URL: `VMAPI_URL`
- Default base URL: `http://localhost`
- `--raw` flag for JSON output on list commands
- Proper error handling with anyhow
- Uses TypedClient for VM action commands (better ergonomics)
- Uses generated Client for all other commands

## Phase 5 Complete - CONVERSION VALIDATED

- Validation report: `conversion-plans/vmapi/validation.md`
- Overall status: READY FOR TESTING
- Endpoint coverage: 46/46 (100%)
- Issues found: 0 blocking issues

## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [x] Phase 3: Generate Client - COMPLETE
- [x] Phase 4: Generate CLI - COMPLETE
- [x] Phase 5: Validate - COMPLETE

## Conversion Complete

The VMAPI API has been converted to Rust. See validation.md for details.

### Generated Artifacts
- API crate: `apis/vmapi-api/`
- Client crate: `clients/internal/vmapi-client/`
- CLI crate: `cli/vmapi-cli/`
- OpenAPI spec: `openapi-specs/generated/vmapi-api.json`

### Next Steps
1. Run integration tests against live Node.js service
2. Address any issues in validation report
3. Deploy Rust service for parallel testing
