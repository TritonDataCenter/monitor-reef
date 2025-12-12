<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# CloudAPI Conversion Plan

## Source
- Path: `/Users/nshalman/Workspace/monitor-reef/target/sdc-cloudapi/`
- Version: 9.20.0
- Package name: cloudapi
- Description: Triton CloudAPI

## Endpoints Summary
- Total: 161 endpoints (counting GET/HEAD pairs separately)
- By method: GET: 59, HEAD: 49, POST: 30, PUT: 12, DELETE: 11
- Source files: 23 files (lib/*.js + lib/endpoints/*.js)

## Endpoints Detail

### Account (from lib/account.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account | GetAccount | |
| HEAD | /:account | HeadAccount | |
| POST | /:account | UpdateAccount | |
| GET | /:account/limits | GetProvisioningLimits | |

### Audit (from lib/audit.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/machines/:machine/audit | MachineAudit | |
| HEAD | /:account/machines/:machine/audit | HeadAudit | |

### Datacenters (from lib/datacenters.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/datacenters | ListDatacenters | |
| GET | /:account/datacenters/:dc | GetDatacenter | |
| GET | /:account/foreigndatacenters | ListForeignDatacenters | |
| POST | /:account/foreigndatacenters | AddForeignDatacenter | |

### Images/Datasets (from lib/datasets.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/images | ListImages | Versioned: 7.0.0-9.0.0 |
| HEAD | /:account/images | HeadImages | Versioned: 7.0.0-9.0.0 |
| GET | /:account/images/:dataset | GetImage | Versioned: 7.0.0-9.0.0 |
| HEAD | /:account/images/:dataset | HeadImage | Versioned: 7.0.0-9.0.0 |
| POST | /:account/images | CreateImageFromMachine | Versioned: 7.0.0-9.0.0 |
| POST | /:account/images/:dataset | UpdateImage | Action-based, see below |
| DELETE | /:account/images/:dataset | DeleteImage | Versioned: 7.0.0-9.0.0 |

### Documentation (from lib/docs.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | / | redirect | Redirects to docs |
| GET | /docs/* | redirect | Regex pattern |
| GET | /favicon.ico | favicon | |

### Keys (from lib/keys.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/keys | CreateKey | Multi-content type |
| GET | /:account/keys | ListKeys | |
| HEAD | /:account/keys | HeadKeys | |
| GET | /:account/keys/:name | GetKey | |
| HEAD | /:account/keys/:name | HeadKey | |
| DELETE | /:account/keys/:name | DeleteKey | |
| POST | /:account/users/:user/keys | CreateUserKey | Multi-content type |
| GET | /:account/users/:user/keys | ListUserKeys | Versioned: 7.2.0-9.0.0 |
| HEAD | /:account/users/:user/keys | HeadUserKeys | Versioned: 7.2.0-9.0.0 |
| GET | /:account/users/:user/keys/:name | GetUserKey | Versioned: 7.2.0-9.0.0 |
| HEAD | /:account/users/:user/keys/:name | HeadUserKey | Versioned: 7.2.0-9.0.0 |
| DELETE | /:account/users/:user/keys/:name | DeleteUserKey | Versioned: 7.2.0-9.0.0 |

### Machines (from lib/machines.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/machines | CreateMachine | |
| POST | /:account/machines/:machine | UpdateMachine | Action-based, see below |
| GET | /:account/machines | ListMachines | |
| HEAD | /:account/machines | HeadMachines | |
| GET | /:account/fwrules/:id/machines | ListFirewallRuleMachines | |
| HEAD | /:account/fwrules/:id/machines | HeadFirewallRuleMachines | |
| GET | /:account/machines/:machine | GetMachine | |
| HEAD | /:account/machines/:machine | HeadMachine | |
| DELETE | /:account/machines/:machine | DeleteMachine | |

### Metadata (from lib/metadata.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/machines/:machine/metadata | AddMachineMetadata | |
| GET | /:account/machines/:machine/metadata | ListMachineMetadata | |
| HEAD | /:account/machines/:machine/metadata | HeadMachineMetadata | |
| GET | /:account/machines/:machine/metadata/:key | GetMachineMetadata | |
| HEAD | /:account/machines/:machine/metadata/:key | HeadMachineMetadata | |
| DELETE | /:account/machines/:machine/metadata | DeleteAllMachineMetadata | |
| DELETE | /:account/machines/:machine/metadata/:key | DeleteMachineMetadata | |

### Migrations (from lib/migrations.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/migrations | ListMigrations | |
| GET | /:account/migrations/:machine | GetMigration | |
| GET | /:account/machines/:machine/migrate | MigrateMachineEstimate | |
| POST | /:account/machines/:machine/migrate | Migrate | |

### NICs (from lib/nics.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/machines/:machine/nics | AddNic | Versioned: 7.2.0-9.0.0 |
| GET | /:account/machines/:machine/nics | ListNics | Versioned: 7.2.0-9.0.0 |
| HEAD | /:account/machines/:machine/nics | ListNics | Versioned: 7.2.0-9.0.0 |
| GET | /:account/machines/:machine/nics/:mac | GetNic | Versioned: 7.2.0-9.0.0 |
| HEAD | /:account/machines/:machine/nics/:mac | GetNic | Versioned: 7.2.0-9.0.0 |
| DELETE | /:account/machines/:machine/nics/:mac | RemoveNic | Versioned: 7.2.0-9.0.0 |

### Packages (from lib/packages.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/packages | ListPackages | |
| HEAD | /:account/packages | HeadPackages | |
| GET | /:account/packages/:package | GetPackage | |
| HEAD | /:account/packages/:package | HeadPackage | |

### Policies (from lib/policies.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/policies | CreatePolicy | Multi-content type |
| GET | /:account/policies | ListPolicies | |
| HEAD | /:account/policies | HeadPolicies | |
| GET | /:account/policies/:policy | GetPolicy | |
| HEAD | /:account/policies/:policy | HeadPolicy | |
| POST | /:account/policies/:policy | UpdatePolicy | |
| DELETE | /:account/policies/:policy | DeletePolicy | |

### Resources/Role Tags (from lib/resources.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| PUT | /:account | ReplaceAccountRoleTags | |
| PUT | /:account/:resource_name | ReplaceResourcesRoleTags | Generic route |
| PUT | /:account/users/:user/:resource_name | ReplaceUserKeysResourcesRoleTags | |
| PUT | /:account/:resource_name/:resource_id | ReplaceResourceRoleTags | Generic route |
| PUT | /:account/machines/:machine | ReplaceMachineRoleTags | |
| PUT | /:account/users/:user/keys/:resource_id | ReplaceUserKeysResourceRoleTags | |

### Roles (from lib/roles.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/roles | CreateRole | Multi-content type |
| GET | /:account/roles | ListRoles | |
| HEAD | /:account/roles | HeadRoles | |
| GET | /:account/roles/:role | GetRole | |
| HEAD | /:account/roles/:role | HeadRole | |
| POST | /:account/roles/:role | UpdateRole | |
| DELETE | /:account/roles/:role | DeleteRole | |

### Firewall Rules (from lib/rules.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/fwrules | CreateFirewallRule | |
| POST | /:account/fwrules/:id | UpdateFirewallRule | |
| GET | /:account/fwrules | ListFirewallRules | |
| HEAD | /:account/fwrules | HeadFirewallRules | |
| GET | /:account/fwrules/:id | GetFirewallRule | |
| HEAD | /:account/fwrules/:id | HeadFirewallRule | |
| POST | /:account/fwrules/:id/enable | EnableFirewallRule | |
| POST | /:account/fwrules/:id/disable | DisableFirewallRule | |
| DELETE | /:account/fwrules/:id | DeleteFirewallRule | |
| GET | /:account/machines/:machine/fwrules | ListMachineFirewallRules | |
| HEAD | /:account/machines/:machine/fwrules | HeadMachineFirewallRules | |

### Services (from lib/services.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/services | ListServices | |

### Snapshots (from lib/snapshots.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/machines/:machine/snapshots | CreateMachineSnapshot | |
| POST | /:account/machines/:machine/snapshots/:name | StartMachineFromSnapshot | |
| GET | /:account/machines/:machine/snapshots | ListMachineSnapshots | |
| HEAD | /:account/machines/:machine/snapshots | HeadMachineSnapshots | |
| GET | /:account/machines/:machine/snapshots/:name | GetMachineSnapshot | |
| HEAD | /:account/machines/:machine/snapshots/:name | HeadMachineSnapshot | |
| DELETE | /:account/machines/:machine/snapshots/:name | DeleteMachineSnapshot | |

### Tags (from lib/tags.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/machines/:machine/tags | AddMachineTags | |
| PUT | /:account/machines/:machine/tags | ReplaceMachineTags | |
| GET | /:account/machines/:machine/tags | ListMachineTags | |
| HEAD | /:account/machines/:machine/tags | HeadmachineTags | |
| GET | /:account/machines/:machine/tags/:tag | GetMachineTag | |
| HEAD | /:account/machines/:machine/tags/:tag | HeadMachineTag | |
| DELETE | /:account/machines/:machine/tags | DeleteMachineTags | |
| DELETE | /:account/machines/:machine/tags/:tag | DeleteMachineTag | |

### Users (from lib/users.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/users | CreateUser | Multi-content type |
| GET | /:account/users | ListUsers | |
| HEAD | /:account/users | HeadUsers | |
| GET | /:account/users/:uuid | GetUser | |
| HEAD | /:account/users/:uuid | HeadUser | |
| POST | /:account/users/:uuid | UpdateUser | |
| POST | /:account/users/:uuid/change_password | ChangeUserPassword | |
| DELETE | /:account/users/:uuid | DeleteUser | |

### Changefeed (from lib/changefeed.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/changefeed | changefeed | WebSocket upgrade |

### Access Keys (from lib/endpoints/accesskeys.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/accesskeys | CreateAccessKey | Multi-content type |
| GET | /:account/accesskeys | ListAccessKeys | |
| HEAD | /:account/accesskeys | HeadAccessKeys | |
| GET | /:account/accesskeys/:accesskeyid | GetAccessKey | |
| HEAD | /:account/accesskeys/:accesskeyid | HeadAccessKey | |
| DELETE | /:account/accesskeys/:accesskeyid | DeleteAccessKey | |
| POST | /:account/users/:user/accesskeys | CreateUserAccessKey | Multi-content type |
| GET | /:account/users/:user/accesskeys | ListUserAccessKeys | |
| HEAD | /:account/users/:user/accesskeys | HeadUserAccessKeys | |
| GET | /:account/users/:user/accesskeys/:accesskeyid | GetUserAccessKey | |
| HEAD | /:account/users/:user/accesskeys/:accesskeyid | HeadUserAccessKey | |
| DELETE | /:account/users/:user/accesskeys/:accesskeyid | DeleteUserAccessKey | |

### Config (from lib/endpoints/config.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/config | GetConfig | |
| HEAD | /:account/config | HeadConfig | |
| PUT | /:account/config | UpdateConfig | |

### Disks (from lib/endpoints/disks.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /:account/machines/:machine/disks | CreateMachineDisk | Versioned: 7.2.0-9.0.0 |
| GET | /:account/machines/:machine/disks | ListMachineDisks | Versioned: 7.2.0-9.0.0 |
| HEAD | /:account/machines/:machine/disks | ListMachineDisks | Versioned: 7.2.0-9.0.0 |
| GET | /:account/machines/:machine/disks/:disk | GetMachineDisk | Versioned: 7.2.0-9.0.0 |
| HEAD | /:account/machines/:machine/disks/:disk | GetMachineDisk | Versioned: 7.2.0-9.0.0 |
| POST | /:account/machines/:machine/disks/:disk | ResizeMachineDisk | Versioned: 7.2.0-9.0.0 |
| DELETE | /:account/machines/:machine/disks/:disk | DeleteMachineDisk | Versioned: 7.2.0-9.0.0 |

### Fabric Networks (from lib/endpoints/networks.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/fabrics/default/vlans | ListFabricVLANs | Versioned: 7.3.0-9.0.0 |
| HEAD | /:account/fabrics/default/vlans | HeadFabricVLANs | Versioned: 7.3.0-9.0.0 |
| POST | /:account/fabrics/default/vlans | CreateFabricVLAN | Versioned: 7.3.0-9.0.0 |
| PUT | /:account/fabrics/default/vlans/:vlan_id | UpdateFabricVLAN | Versioned: 7.3.0-9.0.0 |
| GET | /:account/fabrics/default/vlans/:vlan_id | GetFabricVLAN | Versioned: 7.3.0-9.0.0 |
| HEAD | /:account/fabrics/default/vlans/:vlan_id | GetFabricVLAN | Versioned: 7.3.0-9.0.0 |
| DELETE | /:account/fabrics/default/vlans/:vlan_id | DeleteFabricVLAN | Versioned: 7.3.0-9.0.0 |
| GET | /:account/fabrics/default/vlans/:vlan_id/networks | ListFabricNetworks | Versioned: 7.3.0-9.0.0 |
| HEAD | /:account/fabrics/default/vlans/:vlan_id/networks | HeadFabricNetworks | Versioned: 7.3.0-9.0.0 |
| POST | /:account/fabrics/default/vlans/:vlan_id/networks | CreateFabricNetwork | Versioned: 7.3.0-9.0.0 |
| GET | /:account/fabrics/default/vlans/:vlan_id/networks/:id | GetFabricNetwork | Versioned: 7.3.0-9.0.0 |
| HEAD | /:account/fabrics/default/vlans/:vlan_id/networks/:id | GetFabricNetwork | Versioned: 7.3.0-9.0.0 |
| PUT | /:account/fabrics/default/vlans/:vlan_id/networks/:id | UpdateFabricNetwork | Versioned: 7.3.0-9.0.0 |
| DELETE | /:account/fabrics/default/vlans/:vlan_id/networks/:id | DeleteFabricNetwork | Versioned: 7.3.0-9.0.0 |
| GET | /:account/networks | ListNetworks | |
| HEAD | /:account/networks | HeadNetworks | |
| GET | /:account/networks/:network | GetNetwork | |
| HEAD | /:account/networks/:network | HeadNetwork | |
| GET | /:account/networks/:id/ips | ListNetworkIPs | |
| HEAD | /:account/networks/:id/ips | HeadNetworkIPs | |
| PUT | /:account/networks/:id/ips/:ip_address | UpdateNetworkIP | |
| GET | /:account/networks/:id/ips/:ip_address | GetNetworkIP | |
| HEAD | /:account/networks/:id/ips/:ip_address | HeadNetworkIP | |

### VNC (from lib/endpoints/vnc.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/machines/:machine/vnc | ConnectMachineVNC | Versioned: 8.4.0, WebSocket |

### Volumes (from lib/endpoints/volumes.js)
| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /:account/volumesizes | ListVolumeSizes | |
| GET | /:account/volumes/:id | GetVolume | |
| GET | /:account/volumes | ListVolumes | |
| POST | /:account/volumes | CreateVolume | |
| DELETE | /:account/volumes/:id | DeleteVolume | |
| POST | /:account/volumes/:id | UpdateVolume | |

## Route Conflicts

### No Conflicts Detected

After analyzing all routes, no Dropshot-incompatible conflicts were found. The API design is generally compatible with Dropshot's routing:

- Literal segments like `fwrules/:id/enable` and `fwrules/:id/disable` are fine (literals after variables)
- The `fabrics/default/vlans` path uses "default" as a hardcoded literal, not conflicting with variables
- No patterns like `GET /resource/literal` vs `GET /resource/:variable` exist

## Action Dispatch Endpoints

### POST /:account/machines/:machine (UpdateMachine)

**Supported Actions:**
| Action | Required Fields | Optional Fields | Notes |
|--------|-----------------|-----------------|-------|
| start | (none) | origin | Start a stopped machine |
| stop | (none) | origin | Stop a running machine |
| reboot | (none) | origin | Reboot a running machine |
| resize | package | origin | Resize to different package; not supported for KVM |
| rename | name | origin | Change machine alias; max 189 chars (63 if CNS enabled) |
| enable_firewall | (none) | origin | Enable firewall for machine |
| disable_firewall | (none) | origin | Disable firewall for machine |
| enable_deletion_protection | (none) | origin | Enable deletion protection |
| disable_deletion_protection | (none) | origin | Disable deletion protection |

**Notes:**
- All actions use `req.params.action` to dispatch
- All actions support `origin` parameter (defaults to 'cloudapi')
- Resize validates package compatibility with image requirements
- Rename has length restrictions (189 chars default, 63 with CNS)
- Actions check machine state (reject if deleted)

### POST /:account/images/:dataset (UpdateImage)

**Supported Actions:**
| Action | Required Fields | Optional Fields | Notes |
|--------|-----------------|-----------------|-------|
| update | (at least one attribute) | name, version, description, homepage, eula, acl, tags | Update image metadata |
| export | manta_path | (none) | Export image to Manta |
| clone | (none) | (none) | Clone image to account |
| import-from-datacenter | datacenter, id | (none) | Import image from another datacenter |

**Notes:**
- Actions use `req.params.action` for dispatch
- `update` action requires at least one valid attribute to update
- `export` requires Manta path for destination
- `import-from-datacenter` uses datacenter and id from params

## Planned File Structure

Given the large number of endpoints (161), the API should be split into logical modules:

```
apis/cloudapi-api/src/
├── lib.rs                    # Main trait definition, re-exports
├── types/
│   ├── mod.rs                # Common types re-exports
│   ├── account.rs            # Account, limits types
│   ├── machine.rs            # Machine, CreateMachineRequest, etc.
│   ├── image.rs              # Image/dataset types
│   ├── network.rs            # Network, NIC, fabric types
│   ├── volume.rs             # Volume types
│   ├── firewall.rs           # Firewall rule types
│   ├── user.rs               # User, role, policy types
│   ├── key.rs                # SSH key, access key types
│   └── common.rs             # Shared types (UUIDs, timestamps, etc.)
├── endpoints/
│   ├── mod.rs
│   ├── account.rs            # Account endpoints trait methods
│   ├── machines.rs           # Machine CRUD + actions
│   ├── machine_resources.rs # Tags, metadata, snapshots, audit
│   ├── images.rs             # Image endpoints
│   ├── networks.rs           # Networks, fabrics, NICs
│   ├── volumes.rs            # Volume endpoints
│   ├── firewall.rs           # Firewall rules
│   ├── users.rs              # Users, roles, policies
│   ├── keys.rs               # SSH keys, access keys
│   ├── disks.rs              # Disk management
│   ├── config.rs             # Config endpoint
│   └── misc.rs               # Services, datacenters, changefeed, docs
└── actions/
    ├── mod.rs
    ├── machine_actions.rs    # Machine action request types
    └── image_actions.rs      # Image action request types
```

## Types to Define

### Core Resource Types
- `Account` - Account details with limits
- `Machine` - Virtual machine representation
- `Image` - Image/dataset information
- `Package` - Package/billing plan
- `Network` - Network configuration
- `Volume` - Storage volume
- `FirewallRule` - Firewall rule
- `User` - Sub-user account
- `Role` - RBAC role
- `Policy` - RBAC policy
- `SshKey` - SSH public key
- `AccessKey` - S3-style access key
- `Snapshot` - Machine snapshot
- `Disk` - Flexible disk
- `Nic` - Network interface card
- `VNC` - VNC connection info
- `Datacenter` - Datacenter information

### Request Types
- `CreateMachineRequest` - Machine provisioning parameters
- `MachineActionRequest` - Action dispatch (enum with variants)
- `CreateImageRequest` - Image creation from machine
- `ImageActionRequest` - Image action dispatch (enum with variants)
- `CreateVolumeRequest` - Volume creation parameters
- `CreateFirewallRuleRequest` - Firewall rule creation
- `UpdateFirewallRuleRequest` - Firewall rule updates
- `CreateUserRequest` - User creation
- `CreateRoleRequest` - Role creation
- `CreatePolicyRequest` - Policy creation
- `CreateSshKeyRequest` - SSH key upload
- `CreateAccessKeyRequest` - Access key creation
- `CreateFabricVlanRequest` - Fabric VLAN creation
- `CreateFabricNetworkRequest` - Fabric network creation
- `ResizeDiskRequest` - Disk resize parameters

### Action Enums
```rust
pub enum MachineAction {
    Start,
    Stop,
    Reboot,
    Resize { package: String },
    Rename { name: String },
    EnableFirewall,
    DisableFirewall,
    EnableDeletionProtection,
    DisableDeletionProtection,
}

pub enum ImageAction {
    Update {
        name: Option<String>,
        version: Option<String>,
        description: Option<String>,
        homepage: Option<String>,
        eula: Option<String>,
        acl: Option<Vec<String>>,
        tags: Option<HashMap<String, String>>,
    },
    Export {
        manta_path: String,
    },
    Clone,
    ImportFromDatacenter {
        datacenter: String,
        id: String,
    },
}
```

### Response Types
- `MachineList` - List of machines
- `ImageList` - List of images
- `PackageList` - List of packages
- `NetworkList` - List of networks
- `VolumeList` - List of volumes
- `FirewallRuleList` - List of firewall rules
- `UserList` - List of users
- `RoleList` - List of roles
- `PolicyList` - List of policies
- `SshKeyList` - List of SSH keys
- `AccessKeyList` - List of access keys

### Common Types
- `Uuid` - UUID wrapper (newtype)
- `Timestamp` - RFC3339 timestamp
- `Tags` - HashMap of string tags
- `Metadata` - HashMap of metadata
- `RoleTags` - RBAC role tags
- `ProvisioningLimits` - Account limits

## Special Considerations

### Content-Type Handling
Several endpoints accept multiple content types:
- `multipart/form-data`
- `application/octet-stream`
- `application/json`
- `text/plain`

These are used for: Keys, AccessKeys, Policies, Roles, Users

### API Versioning
Many endpoints have version constraints (e.g., `version: ['7.2.0', '7.3.0', '8.0.0', '9.0.0']`).
Dropshot doesn't have built-in versioning, so this will need to be handled at the implementation level or via separate traits.

### WebSocket Endpoints
- `GET /:account/changefeed` - Changefeed WebSocket
- `GET /:account/machines/:machine/vnc` - VNC WebSocket

Dropshot has WebSocket support via the `websocket` feature. These will need special handling.

### Generic Resource Routes
The resources module has generic routes like:
- `PUT /:account/:resource_name/:resource_id` - ReplaceResourceRoleTags

These may need careful handling in Dropshot to ensure they don't conflict with specific routes.

### Regex Routes
`GET /docs/*` uses a regex pattern. Dropshot doesn't support regex routes, so this should be converted to a catch-all or specific routes.

## Migration Notes

1. **Action Dispatch Pattern**: The machine and image update endpoints use an action query parameter. In Rust, these should be separate endpoints or use an enum-based approach.

2. **Large API Surface**: With 161 endpoints, this is one of the larger Triton APIs. Module organization will be critical.

3. **RBAC Integration**: Many endpoints integrate with the Aperture RBAC system via role tags. The trait design should support this.

4. **Plugin Hooks**: The machine creation has pre/post hooks for plugins. The trait should support middleware-style hooks.

5. **Multi-Content Type**: Several create endpoints support multiple content types. Dropshot handles this via the `content_type` parameter.

6. **Versioning Strategy**: API versioning is embedded in route definitions. Consider whether to:
   - Create separate traits per version
   - Handle versioning in the implementation
   - Use a hybrid approach

## Phase 2 Complete - API Trait Generated

- **API crate**: `apis/cloudapi-api/`
- **OpenAPI spec**: `openapi-specs/generated/cloudapi-api.json` (255KB)
- **Endpoint count**: 158 endpoints (4 generic resource role tag endpoints omitted due to Dropshot routing conflicts)
- **Build status**: SUCCESS

### Changes from Original API

#### Omitted Endpoints (Dropshot Route Conflicts)

The following 4 generic resource role tag endpoints were omitted because they conflict with literal path segments in Dropshot's routing:

1. `PUT /:account/:resource_name` - ReplaceResourcesRoleTags
2. `PUT /:account/:resource_name/:resource_id` - ReplaceResourceRoleTags
3. `PUT /:account/users/:user/:resource_name` - ReplaceUserKeysResourcesRoleTags
4. (Merged) `PUT /:account/users/:user/keys/:resource_id` - Merged into existing user key endpoint

**Rationale**: Dropshot does not support having both variable path segments (e.g., `{resource_name}`) and literal path segments (e.g., `machines`, `users`) at the same level. The generic endpoints would match any resource type, but this conflicts with all the specific resource endpoints like `/machines`, `/images`, `/networks`, etc.

**Impact**: The specific endpoints remain available:
- `PUT /:account/machines/:machine` - ReplaceMachineRoleTags (kept)
- `PUT /:account/users/:uuid/keys/:name` - Replace user key role tags (kept)

If additional resource types need role tag support, specific endpoints can be added (e.g., `PUT /:account/images/:image` for image role tags).

### Path Parameter Naming Fixes

Several path parameter names were standardized to avoid Dropshot conflicts:
- User endpoints: Standardized on `{uuid}` (was mixed `{user}` and `{uuid}`)
- Network IP endpoints: Standardized on `{network}` (was `{id}`)
- User key/accesskey sub-resources: All use `{uuid}` to match parent user routes

### Module Structure

```
apis/cloudapi-api/src/
├── lib.rs                     # CloudAPI trait with 158 endpoints
├── types/
│   ├── mod.rs                 # Re-exports all type modules
│   ├── account.rs             # Account, limits, config types
│   ├── common.rs              # UUID, timestamp, tags, metadata
│   ├── firewall.rs            # Firewall rule types
│   ├── image.rs               # Image/dataset types with action dispatch
│   ├── key.rs                 # SSH key and access key types
│   ├── machine.rs             # Machine types with action dispatch
│   ├── machine_resources.rs   # Snapshots, disks, tags, metadata
│   ├── misc.rs                # Packages, datacenters, services, migrations
│   ├── network.rs             # Networks, fabric VLANs, NICs, IPs
│   ├── user.rs                # Users, roles, policies
│   └── volume.rs              # Volume types with action dispatch
```

### Action Dispatch Endpoints

Three endpoints use action dispatch pattern (body is `serde_json::Value`, specific request types exported):

1. **Machine Actions** (`POST /:account/machines/:machine?action=...`):
   - `start`, `stop`, `reboot`, `resize`, `rename`
   - `enable_firewall`, `disable_firewall`
   - `enable_deletion_protection`, `disable_deletion_protection`

2. **Image Actions** (`POST /:account/images/:dataset?action=...`):
   - `update`, `export`, `clone`, `import-from-datacenter`

3. **Volume Actions** (`POST /:account/volumes/:id?action=...`):
   - `update`

4. **Disk Actions** (`POST /:account/machines/:machine/disks/:disk?action=...`):
   - `resize`

Each action has a dedicated typed request struct (e.g., `StartMachineRequest`, `StopMachineRequest`, `ResizeMachineRequest`, etc.) exported from the API crate for use by client libraries.

## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [ ] Phase 3: Generate Client
- [ ] Phase 4: Generate CLI
- [ ] Phase 5: Validate
