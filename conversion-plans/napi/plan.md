# NAPI API Conversion Plan

## Source
- Path: ./target/sdc-napi
- Version: 1.4.3
- Package name: napi
- Description: Triton Networking API

## Endpoints Summary
- Total: 48 (excluding HEAD duplicates: 34 unique handlers)
- By method: GET: 17, HEAD: 13, POST: 7, PUT: 7, DELETE: 6
- Source files:
  - `lib/endpoints/ping.js`
  - `lib/endpoints/nics.js`
  - `lib/endpoints/nic-tags.js`
  - `lib/endpoints/network-pools.js`
  - `lib/endpoints/aggregations.js`
  - `lib/endpoints/search.js`
  - `lib/endpoints/manage.js`
  - `lib/endpoints/networks/index.js`
  - `lib/endpoints/networks/ips.js`
  - `lib/endpoints/networks/nics.js`
  - `lib/endpoints/fabrics/vlans.js`
  - `lib/endpoints/fabrics/networks.js`

## Endpoints Detail

### Ping (from `lib/endpoints/ping.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /ping | ping | 200 `PingResponse` | Bypasses `checkServices` middleware |
| HEAD | /ping | ping | 200 `PingResponse` | |

### NICs (from `lib/endpoints/nics.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /nics | listNics | 200 `Vec<Nic>` | |
| HEAD | /nics | listNics | 200 `Vec<Nic>` | |
| POST | /nics | postNic | 200 `Nic` | Note: 200, NOT 201 |
| GET | /nics/:mac | getNic | 200 `Nic` | MAC can be colon-separated, dash-separated, or bare hex |
| HEAD | /nics/:mac | getNic | 200 `Nic` | |
| PUT | /nics/:mac | putNic | 200 `Nic` | |
| DELETE | /nics/:mac | deleteNic | 204 | Supports conditional request (If-Match) |

### NIC Tags (from `lib/endpoints/nic-tags.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /nic_tags | listNicTags | 200 `Vec<NicTag>` | |
| HEAD | /nic_tags | listNicTags | 200 `Vec<NicTag>` | |
| POST | /nic_tags | postNicTag | 200 `NicTag` | Note: 200, NOT 201 |
| GET | /nic_tags/:name | getNicTag | 200 `NicTag` | |
| HEAD | /nic_tags/:name | getNicTag | 200 `NicTag` | |
| PUT | /nic_tags/:oldname | putNicTag | 200 `NicTag` | Note: param is `:oldname` |
| DELETE | /nic_tags/:name | deleteNicTag | 204 | |

### Network Pools (from `lib/endpoints/network-pools.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /network_pools | listNetworkPools | 200 `Vec<NetworkPool>` | |
| HEAD | /network_pools | listNetworkPools | 200 `Vec<NetworkPool>` | |
| POST | /network_pools | postNetworkPool | 200 `NetworkPool` | Note: 200, NOT 201 |
| GET | /network_pools/:uuid | getNetworkPool | 200 `NetworkPool` | |
| HEAD | /network_pools/:uuid | getNetworkPool | 200 `NetworkPool` | |
| PUT | /network_pools/:uuid | putNetworkPool | 200 `NetworkPool` | |
| DELETE | /network_pools/:uuid | deleteNetworkPool | 204 | |

### Aggregations (from `lib/endpoints/aggregations.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /aggregations | listAggregations | 200 `Vec<Aggregation>` | |
| HEAD | /aggregations | listAggregations | 200 `Vec<Aggregation>` | |
| POST | /aggregations | postAggregation | 200 `Aggregation` | Note: 200, NOT 201 |
| GET | /aggregations/:id | getAggregation | 200 `Aggregation` | id is `{belongs_to_uuid}-{name}` |
| HEAD | /aggregations/:id | getAggregation | 200 `Aggregation` | |
| PUT | /aggregations/:id | putAggregation | 200 `Aggregation` | |
| DELETE | /aggregations/:id | deleteAggregation | 204 | |

### Networks (from `lib/endpoints/networks/index.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /networks | listNetworks | 200 `Vec<Network>` | |
| HEAD | /networks | listNetworks | 200 `Vec<Network>` | |
| POST | /networks | postNetwork | 200 `Network` | Note: 200, NOT 201 |
| GET | /networks/:uuid | getNetwork | 200 `Network` | Also accepts "admin" as symbolic name |
| HEAD | /networks/:uuid | getNetwork | 200 `Network` | |
| PUT | /networks/:uuid | putNetwork | 200 `Network` | |
| DELETE | /networks/:uuid | deleteNetwork | 204 | Checks network isn't in a pool first |

### Network IPs (from `lib/endpoints/networks/ips.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /networks/:network_uuid/ips | listIPs | 200 `Vec<Ip>` | |
| HEAD | /networks/:network_uuid/ips | listIPs | 200 `Vec<Ip>` | |
| GET | /networks/:network_uuid/ips/:ip_addr | getIP | 200 `Ip` | Returns object even if IP not in moray (pretend it exists) |
| PUT | /networks/:network_uuid/ips/:ip_addr | putIP | 200 `Ip` | Creates or updates IP; special `free` param deletes |

### Network NICs (from `lib/endpoints/networks/nics.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| POST | /networks/:network_uuid/nics | postNetworkNic | 200 `Nic` | Provision a NIC on a specific network |

### Search (from `lib/endpoints/search.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /search/ips | searchIPs | 200 `Vec<Ip>` | Search for IP across all networks; returns 404 if not found |

### Manage (from `lib/endpoints/manage.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /manage/gc | runGC | 200 `GcResponse` | Returns 501 if GC not exposed |
| HEAD | /manage/gc | runGC | 200 `GcResponse` | |

### Fabric VLANs (from `lib/endpoints/fabrics/vlans.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /fabrics/:owner_uuid/vlans | listFabricVLANs | 200 `Vec<FabricVlan>` | Requires overlays enabled |
| POST | /fabrics/:owner_uuid/vlans | createFabricVLAN | 200 `FabricVlan` | Note: 200, NOT 201 |
| GET | /fabrics/:owner_uuid/vlans/:vlan_id | getFabricVLAN | 200 `FabricVlan` | |
| PUT | /fabrics/:owner_uuid/vlans/:vlan_id | updateFabricVLAN | 200 `FabricVlan` | |
| DELETE | /fabrics/:owner_uuid/vlans/:vlan_id | delFabricVLAN | 204 | Checks no networks on VLAN first |

### Fabric Networks (from `lib/endpoints/fabrics/networks.js`)
| Method | Path | Handler | Response | Notes |
|--------|------|---------|----------|-------|
| GET | /fabrics/:owner_uuid/vlans/:vlan_id/networks | listFabricNetworks | 200 `Vec<Network>` | Serialized with `{ fabric: true }` option |
| POST | /fabrics/:owner_uuid/vlans/:vlan_id/networks | createFabricNetwork | 200 `Network` | Auto-sets mtu, nic_tag, vnet_id; Note: 200 NOT 201 |
| GET | /fabrics/:owner_uuid/vlans/:vlan_id/networks/:uuid | getFabricNetwork | 200 `Network` | |
| PUT | /fabrics/:owner_uuid/vlans/:vlan_id/networks/:uuid | updateFabricNetwork | 200 `Network` | |
| DELETE | /fabrics/:owner_uuid/vlans/:vlan_id/networks/:uuid | delFabricNetwork | 204 | |

## Route Conflicts

No route conflicts detected. All paths use distinct literal segments or the parameterized segments are unambiguous. The network GET endpoint accepts "admin" as a symbolic name for `:uuid`, but this is handled in the model layer (not a route conflict).

**Status: RESOLVED** -- No conflicts.

## Action Dispatch Endpoints

None. NAPI does not use the action dispatch pattern. All operations are separate endpoints with distinct HTTP methods and paths.

## Planned File Structure
```
apis/napi-api/src/
  lib.rs          -- Re-exports, main NapiApi trait
  types/
    mod.rs        -- Common types module
    ping.rs       -- PingResponse, PingConfig, PingServices
    nic.rs        -- Nic, NicState, BelongsToType, NIC boolean/optional fields
    nic_tag.rs    -- NicTag
    network.rs    -- Network, NetworkFamily
    network_pool.rs -- NetworkPool
    ip.rs         -- Ip (IP address record)
    aggregation.rs  -- Aggregation, LacpMode
    fabric.rs     -- FabricVlan
    search.rs     -- SearchIpsQuery
    manage.rs     -- GcResponse
```

## Enum Opportunities

### NicState
- Field: `Nic.state`
- Variants: `provisioning`, `stopped`, `running`
- Source: `VALID_NIC_STATES` in `lib/models/nic/common.js`
- Default: `provisioning` (see `constants.DEFAULT_NIC_STATE`)
- Needs `#[serde(other)] Unknown`: Yes (server-controlled state field)

### BelongsToType
- Field: `Nic.belongs_to_type`, `Ip.belongs_to_type`
- Variants: `other`, `server`, `zone`
- Source: `BELONGS_TO_TYPES` in `lib/models/nic/common.js`
- Needs `#[serde(other)] Unknown`: Yes

### LacpMode
- Field: `Aggregation.lacp_mode`
- Variants: `off`, `active`, `passive`
- Source: `LACP_MODES` in `lib/models/aggregation.js`
- Default: `off`
- Needs `#[serde(other)] Unknown`: Yes

### NetworkFamily
- Field: `Network.family`, `NetworkPool.family`
- Variants: `ipv4`, `ipv6`
- Source: Various `validate.enum` calls
- Needs `#[serde(other)] Unknown`: Yes

### MorayServiceStatus
- Field: `PingResponse.services.moray`
- Variants: `online`, `offline`
- Source: `lib/endpoints/ping.js`
- Needs `#[serde(other)] Unknown`: Yes

### PingStatus
- Field: `PingResponse.status`
- Variants: `OK`, `initializing`
- Source: `lib/endpoints/ping.js`
- Needs `#[serde(other)] Unknown`: Yes

## Patch Requirements

### POST /nics returns 200 (not 201)
All create endpoints (`POST /nics`, `POST /nic_tags`, `POST /networks`, `POST /network_pools`, `POST /aggregations`, `POST /fabrics/:owner_uuid/vlans`, `POST /fabrics/:owner_uuid/vlans/:vlan_id/networks`, `POST /networks/:network_uuid/nics`) return **200**, not 201. Use `HttpResponseOk<T>` in the API trait.

### GET /manage/gc returns 200 or 501
The handler returns 200 with a JSON body on success, or 501 `NotImplementedError` if GC is not exposed. The 501 is an error path, not a dual-response. Use `HttpResponseOk<GcResponse>` and let 501 be an error.

### GET /search/ips returns 404 if no results
Returns `ResourceNotFoundError` when no results found, otherwise 200 with array. The 404 is an error, so use `HttpResponseOk<Vec<Ip>>`.

## Types to Define

### PingResponse
```
{
  config: {
    fabrics_enabled: bool,
    subnet_alloc_enabled: bool
  },
  healthy: bool,
  services: {
    moray: MorayServiceStatus  // "online" | "offline"
  },
  status: PingStatus  // "OK" | "initializing"
}
```

### NicTag
```
{
  mtu: u32,
  name: String,
  uuid: Uuid
}
```

### NicTag Create Body
```
{
  name: String (required),
  uuid: Option<Uuid>,
  mtu: Option<u32>
}
```

### NicTag Update Body
```
{
  name: Option<String>,
  mtu: Option<u32>
}
```

### Nic
```
{
  belongs_to_type: BelongsToType,
  belongs_to_uuid: Uuid,
  mac: String,  // MAC address as colon-separated string
  owner_uuid: Uuid,
  primary: bool,
  state: NicState,
  created_timestamp: String,  // ISO 8601
  modified_timestamp: String,  // ISO 8601
  ip: Option<String>,  // IP address string
  // Fields from network (read-only, optional):
  fabric: Option<bool>,
  gateway: Option<String>,
  gateway_provisioned: Option<bool>,
  internet_nat: Option<bool>,
  mtu: Option<u32>,
  netmask: Option<String>,
  nic_tag: Option<String>,
  resolvers: Option<Vec<String>>,
  vlan_id: Option<u32>,
  network_uuid: Option<Uuid>,
  routes: Option<HashMap<String, String>>,
  // Optional params:
  cn_uuid: Option<Uuid>,
  model: Option<String>,
  nic_tags_provided: Option<Vec<String>>,
  // Boolean params (only present when true):
  allow_dhcp_spoofing: Option<bool>,
  allow_ip_spoofing: Option<bool>,
  allow_mac_spoofing: Option<bool>,
  allow_restricted_traffic: Option<bool>,
  allow_unfiltered_promisc: Option<bool>,
  underlay: Option<bool>
}
```

### Nic Create Body
```
{
  belongs_to_uuid: Uuid (required),
  belongs_to_type: BelongsToType (required),
  owner_uuid: Uuid (required),
  // Optional:
  allow_dhcp_spoofing: Option<bool>,
  allow_ip_spoofing: Option<bool>,
  allow_mac_spoofing: Option<bool>,
  allow_restricted_traffic: Option<bool>,
  allow_unfiltered_promisc: Option<bool>,
  check_owner: Option<bool>,
  cn_uuid: Option<Uuid>,
  ip: Option<String>,
  mac: Option<String>,
  model: Option<String>,
  network_uuid: Option<Uuid>,
  nic_tag: Option<String>,
  nic_tags_available: Option<Vec<String>>,
  nic_tags_provided: Option<Vec<String>>,
  primary: Option<bool>,
  reserved: Option<bool>,
  state: Option<NicState>,
  underlay: Option<bool>,
  vlan_id: Option<u32>
}
```

### Network
```
{
  family: NetworkFamily,
  mtu: u32,
  nic_tag: String,
  name: String,
  provision_end_ip: String,
  provision_start_ip: String,
  subnet: String,
  uuid: Uuid,
  vlan_id: u32,
  resolvers: Vec<String>,
  // Optional:
  fabric: Option<bool>,
  vnet_id: Option<u32>,
  internet_nat: Option<bool>,
  gateway_provisioned: Option<bool>,
  gateway: Option<String>,
  routes: Option<HashMap<String, String>>,
  description: Option<String>,
  owner_uuids: Option<Vec<Uuid>>,
  owner_uuid: Option<Uuid>,  // Only for fabric serialization
  netmask: Option<String>  // IPv4 only
}
```

### Network Create Body
```
{
  name: String (required),
  nic_tag: String (required),
  vlan_id: u32 (required),
  // Conditionally required (unless subnet_alloc is true):
  subnet: Option<String>,
  provision_start_ip: Option<String>,
  provision_end_ip: Option<String>,
  // Optional:
  description: Option<String>,
  fabric: Option<bool>,
  subnet_alloc: Option<bool>,
  family: Option<NetworkFamily>,
  subnet_prefix: Option<u32>,
  fields: Option<Vec<String>>,
  gateway: Option<String>,
  internet_nat: Option<bool>,
  mtu: Option<u32>,
  owner_uuids: Option<Vec<Uuid>>,
  routes: Option<HashMap<String, String>>,
  resolvers: Option<Vec<String>>,
  uuid: Option<Uuid>,
  vnet_id: Option<u32>
}
```

### NetworkPool
```
{
  family: NetworkFamily,
  uuid: Uuid,
  name: String,
  description: Option<String>,
  networks: Vec<Uuid>,
  nic_tags_present: Option<Vec<String>>,
  nic_tag: Option<String>,  // Backwards compat: first of nic_tags_present
  owner_uuids: Option<Vec<Uuid>>
}
```

### NetworkPool Create Body
```
{
  name: String (required),
  networks: Vec<Uuid> (required),
  description: Option<String>,
  owner_uuids: Option<Vec<Uuid>>,
  uuid: Option<Uuid>
}
```

### Ip (IP address record)
```
{
  ip: String,
  network_uuid: Uuid,
  reserved: bool,
  free: bool,
  // Optional (present when IP is assigned):
  belongs_to_type: Option<String>,
  belongs_to_uuid: Option<Uuid>,
  owner_uuid: Option<Uuid>
}
```

### Ip Update Body (PUT /networks/:network_uuid/ips/:ip_addr)
```
{
  belongs_to_type: Option<String>,
  belongs_to_uuid: Option<Uuid>,
  check_owner: Option<bool>,
  owner_uuid: Option<Uuid>,
  reserved: Option<bool>,
  free: Option<bool>,
  unassign: Option<bool>
}
```

### Aggregation
```
{
  belongs_to_uuid: Uuid,
  id: String,
  lacp_mode: LacpMode,
  name: String,
  macs: Vec<String>,  // MAC addresses as colon-separated strings
  nic_tags_provided: Option<Vec<String>>
}
```

### Aggregation Create Body
```
{
  name: String (required),
  macs: Vec<String> (required),
  lacp_mode: Option<LacpMode>,
  nic_tags_provided: Option<Vec<String>>
}
```

### FabricVlan
```
{
  name: Option<String>,
  owner_uuid: Uuid,
  vlan_id: u32,
  vnet_id: u32,
  description: Option<String>
}
```

### FabricVlan Create Body
```
{
  owner_uuid: Uuid (from path),
  vlan_id: u32 (required in body),
  name: Option<String>,
  description: Option<String>,
  fields: Option<Vec<String>>
}
```

### SearchIpsQuery
```
{
  ip: String (required),
  belongs_to_type: Option<String>,
  belongs_to_uuid: Option<Uuid>,
  fabric: Option<bool>,
  owner_uuid: Option<Uuid>
}
```

### GcResponse
```
{
  start: MemoryUsage,
  end: MemoryUsage
}
// where MemoryUsage = { rss: u64, heapTotal: u64, heapUsed: u64, time: u64 }
```

## Field Naming Exceptions

**All field names are snake_case** in the wire format. NAPI's JSON API uses snake_case consistently throughout -- there is no camelCase convention. This means the Rust structs should **NOT** use `#[serde(rename_all = "camelCase")]`. Use the default (no rename_all), and all field names will serialize as-is in snake_case.

Specific field naming notes:
- `belongs_to_uuid` -- snake_case
- `belongs_to_type` -- snake_case
- `owner_uuid` -- snake_case
- `owner_uuids` -- snake_case
- `network_uuid` -- snake_case
- `nic_tag` -- snake_case
- `nic_tags_provided` -- snake_case
- `nic_tags_present` -- snake_case
- `vlan_id` -- snake_case
- `vnet_id` -- snake_case
- `lacp_mode` -- snake_case
- `provision_start_ip` -- snake_case
- `provision_end_ip` -- snake_case
- `allow_dhcp_spoofing` -- snake_case
- `allow_ip_spoofing` -- snake_case
- `allow_mac_spoofing` -- snake_case
- `allow_restricted_traffic` -- snake_case
- `allow_unfiltered_promisc` -- snake_case
- `gateway_provisioned` -- snake_case
- `internet_nat` -- snake_case
- `subnet_alloc_enabled` -- snake_case (in PingResponse.config)
- `fabrics_enabled` -- snake_case (in PingResponse.config)
- `created_timestamp` -- snake_case (ISO 8601 string)
- `modified_timestamp` -- snake_case (ISO 8601 string)
- `ip_addr` -- path parameter (not a JSON field)
- `free_space` -- not applicable to NAPI

**Exception -- GcResponse fields**: The GcResponse contains `heapTotal` and `heapUsed` from Node.js `process.memoryUsage()`, which are camelCase. These need explicit `#[serde(rename = "heapTotal")]` and `#[serde(rename = "heapUsed")]`.

## WebSocket/Channel Endpoints

None. NAPI does not have WebSocket endpoints. The changefeed module registers its own listener route (`GET /changefeeds`) on the restify server, but this is handled by the `changefeed` npm module, not by NAPI endpoint code. The changefeed route is not part of NAPI's public API contract and should not be included in the Dropshot trait.

## List Query Parameters

Many list endpoints share common pagination parameters:

| Parameter | Type | Default | Notes |
|-----------|------|---------|-------|
| `limit` | integer | 1000 | Must be 1-1000 |
| `offset` | integer | 0 | Must be >= 0 |

### GET /nics query parameters
- `allow_dhcp_spoofing`, `allow_ip_spoofing`, `allow_mac_spoofing`, `allow_restricted_traffic`, `allow_unfiltered_promisc`: `bool`
- `owner_uuid`, `cn_uuid`, `belongs_to_uuid`, `network_uuid`: `UUID[]` (array for multi-value filter)
- `belongs_to_type`, `nic_tag`, `nic_tags_provided`: `String[]`
- `state`: `String`
- `underlay`: `bool`

### GET /networks query parameters
- `uuid`: UUID prefix filter
- `fabric`: `bool`
- `family`: `NetworkFamily` enum
- `name`: `String[]`
- `nic_tag`: `String[]`
- `owner_uuid`: `UUID`
- `provisionable_by`: `UUID`
- `vlan_id`: `u32`

### GET /network_pools query parameters
- `uuid`: UUID prefix filter
- `name`: `String`
- `family`: `NetworkFamily` enum
- `networks`: `UUID` (single UUID only)
- `provisionable_by`: `UUID`

### GET /aggregations query parameters
- `belongs_to_uuid`: `UUID`
- `macs`: `MAC[]`
- `nic_tags_provided`: `String[]`

### GET /fabrics/:owner_uuid/vlans query parameters
- `fields`: `Vec<String>` (field selection filter)

### GET /networks/:network_uuid/ips query parameters
- `belongs_to_type`: `String`
- `belongs_to_uuid`: `UUID`
- `owner_uuid`: `UUID`

### GET /search/ips query parameters
- `ip`: `String` (required)
- `belongs_to_type`: `String`
- `belongs_to_uuid`: `UUID`
- `fabric`: `bool`
- `owner_uuid`: `UUID`

## Hidden Optional Fields

### NIC creation accepts optional `mac`
Callers can specify a MAC address; if omitted, one is auto-generated.

### Network creation accepts optional `uuid`
Callers can specify a UUID; if omitted, one is auto-generated.

### Network pool creation accepts optional `uuid`
Same pattern as networks.

### NIC tag creation accepts optional `uuid`
Same pattern.

### Network GET accepts "admin" as symbolic name
The network UUID parameter can be the literal string "admin" to look up the admin network.

### IP PUT with `free=true`
Setting `free=true` on a PUT to an IP address triggers a delete (unassign) of the IP, rather than an update. The `unassign` parameter is mutually exclusive with `free`.

### Fields query parameter (fabrics)
Fabric VLAN and Network endpoints accept a `fields` query parameter to select which fields appear in the response (field projection).

## Changefeed Integration

NAPI publishes changefeed events for these resources:
- `aggregation`: subResources: `create`, `delete`, `lacp_mode`
- `network`: subResources: `create`, `delete`, `gateway`, `resolvers`, `routes`
- `nic_tag`: subResources: `create`, `delete`
- `nic`: subResources: `create`, `delete`, `allow_dhcp_spoofing`, `allow_ip_spoofing`, `allow_mac_spoofing`, `allow_restricted_traffic`, `allow_unfiltered_promisc`, `primary`, `state`

The changefeed is published through bootstrap routes: `/aggregations`, `/networks`, `/nic_tags`, `/nics`. These are registered by the `changefeed` npm module, not as explicit NAPI endpoints. In the Rust port, changefeed publishing would be handled by the service implementation, not the API trait.

## Phase 2 Complete

- API crate: `apis/napi-api/`
- OpenAPI spec: `openapi-specs/generated/napi-api.json`
- Endpoint count: 42
- Build status: SUCCESS
- Enums defined: NicState, BelongsToType, LacpMode, NetworkFamily, MorayServiceStatus, PingStatus (all with `#[serde(other)] Unknown`)
- Type modules: common, ping, nic, nic_tag, network, pool, ip, aggregation, fabric, manage
- Notes:
  - Dropshot requires consistent path variable names at the same level, so `/nic_tags/{oldname}` was unified to `/nic_tags/{name}` (PUT uses same path param as GET/DELETE)
  - `/networks/{network_uuid}/ips` and `/networks/{network_uuid}/nics` use `/networks/{uuid}/...` to match `/networks/{uuid}` GET/PUT/DELETE
  - All create endpoints return 200 (HttpResponseOk), not 201
  - All delete endpoints return 204 (HttpResponseUpdatedNoContent)
  - No `#[serde(rename_all = "camelCase")]` -- all wire format is snake_case
  - GcResponse.MemoryUsage has explicit `#[serde(rename = "heapTotal")]` and `#[serde(rename = "heapUsed")]` for Node.js camelCase fields

## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [ ] Phase 3: Generate Client
- [ ] Phase 4: Generate CLI
- [ ] Phase 5: Validate
