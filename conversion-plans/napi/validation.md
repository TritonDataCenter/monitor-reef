# NAPI Conversion Validation Report

## Summary

| Category | Status | Issues |
|----------|--------|--------|
| Endpoint Coverage | OK | 42 of 42 endpoints (34 unique handlers; HEAD excluded) |
| Type Completeness | OK | All response/request types present, 2 minor notes |
| Route Conflicts | OK | No conflicts; path-variable unification documented |
| CLI Coverage | OK | 42 commands covering all endpoints |
| Enum Wire Values | OK | All 6 enums match Node.js source exactly |
| Response Status Codes | OK | All correct (200 for creates, 204 for deletes) |
| API Compatibility | OK | 1 minor concern (NetworkPool.description serialization) |

## Endpoint Coverage

### Converted Endpoints

All 34 unique Node.js handlers (excluding HEAD duplicates) are covered by 42 Rust trait methods and OpenAPI paths.

| Node.js Route | Rust Trait Method | Notes |
|---------------|-------------------|-------|
| `GET /ping` | `ping` | |
| `HEAD /ping` | (Dropshot auto) | HEAD handled by framework |
| `GET /nics` | `list_nics` | |
| `POST /nics` | `create_nic` | |
| `GET /nics/:mac` | `get_nic` | |
| `PUT /nics/:mac` | `update_nic` | |
| `DELETE /nics/:mac` | `delete_nic` | |
| `GET /nic_tags` | `list_nic_tags` | |
| `POST /nic_tags` | `create_nic_tag` | |
| `GET /nic_tags/:name` | `get_nic_tag` | |
| `PUT /nic_tags/:oldname` | `update_nic_tag` | Path unified to `{name}` |
| `DELETE /nic_tags/:name` | `delete_nic_tag` | |
| `GET /networks` | `list_networks` | |
| `POST /networks` | `create_network` | |
| `GET /networks/:uuid` | `get_network` | |
| `PUT /networks/:uuid` | `update_network` | |
| `DELETE /networks/:uuid` | `delete_network` | |
| `GET /networks/:network_uuid/ips` | `list_ips` | Path uses `{uuid}` for consistency |
| `GET /networks/:network_uuid/ips/:ip_addr` | `get_ip` | Path uses `{uuid}` for consistency |
| `PUT /networks/:network_uuid/ips/:ip_addr` | `update_ip` | Path uses `{uuid}` for consistency |
| `POST /networks/:network_uuid/nics` | `create_network_nic` | Path uses `{uuid}` for consistency |
| `GET /network_pools` | `list_network_pools` | |
| `POST /network_pools` | `create_network_pool` | |
| `GET /network_pools/:uuid` | `get_network_pool` | |
| `PUT /network_pools/:uuid` | `update_network_pool` | |
| `DELETE /network_pools/:uuid` | `delete_network_pool` | |
| `GET /aggregations` | `list_aggregations` | |
| `POST /aggregations` | `create_aggregation` | |
| `GET /aggregations/:id` | `get_aggregation` | |
| `PUT /aggregations/:id` | `update_aggregation` | |
| `DELETE /aggregations/:id` | `delete_aggregation` | |
| `GET /fabrics/:owner_uuid/vlans` | `list_fabric_vlans` | |
| `POST /fabrics/:owner_uuid/vlans` | `create_fabric_vlan` | |
| `GET /fabrics/:owner_uuid/vlans/:vlan_id` | `get_fabric_vlan` | |
| `PUT /fabrics/:owner_uuid/vlans/:vlan_id` | `update_fabric_vlan` | |
| `DELETE /fabrics/:owner_uuid/vlans/:vlan_id` | `delete_fabric_vlan` | |
| `GET /fabrics/:owner_uuid/vlans/:vlan_id/networks` | `list_fabric_networks` | |
| `POST /fabrics/:owner_uuid/vlans/:vlan_id/networks` | `create_fabric_network` | |
| `GET /fabrics/:owner_uuid/vlans/:vlan_id/networks/:uuid` | `get_fabric_network` | |
| `PUT /fabrics/:owner_uuid/vlans/:vlan_id/networks/:uuid` | `update_fabric_network` | |
| `DELETE /fabrics/:owner_uuid/vlans/:vlan_id/networks/:uuid` | `delete_fabric_network` | |
| `GET /search/ips` | `search_ips` | |
| `GET /manage/gc` | `run_gc` | |

### Missing Endpoints

None. All HEAD endpoints are handled automatically by Dropshot (it responds to HEAD for any GET endpoint).

### Intentionally Omitted

- `/changefeeds` -- registered by the `changefeed` npm module, not part of NAPI's public API contract

## Type Analysis

### Complete Types

All response and request types are present and field-complete:

- **PingResponse** -- `config`, `healthy`, `services`, `status` all present
- **PingConfig** -- `fabrics_enabled`, `subnet_alloc_enabled`
- **PingServices** -- `moray`
- **Nic** -- All 26 fields match Node.js `serialize()` output
- **NicTag** -- `mtu`, `name`, `uuid` match exactly
- **Network** -- All fields from `networkSerialize()` present including conditional `fabric`, `vnet_id`, `internet_nat`, `gateway_provisioned`, `netmask`, `owner_uuid`, `owner_uuids`
- **NetworkPool** -- All fields match `poolSerialize()`
- **Ip** -- All 7 fields match `ipSerialize()` including conditional `belongs_to_type`, `belongs_to_uuid`, `owner_uuid`
- **Aggregation** -- All fields match `aggrSerialize()`
- **FabricVlan** -- All fields match `fabricVlanSerializer()`
- **GcResponse / MemoryUsage** -- `start`, `end` with `rss`, `heapTotal`, `heapUsed`, `time`
- All Create/Update request body types have correct required/optional fields

### Minor Notes

1. **NetworkPool.description**: In Node.js, `description` is always included in the serialized output (even when `undefined`, which serializes as JSON `null`). In Rust, it is `Option<String>` with `skip_serializing_if = "Option::is_none"`. This means the Rust version omits the field entirely when `None`, while Node.js includes `"description": null`. This is a cosmetic difference -- consumers using `serde_json::Value` would see a missing key vs a null key. For typed consumers this is functionally equivalent because both deserialize to `None`.

2. **FabricVlan.name**: Similarly, Node.js always includes `name` in the non-fields serialization path. Rust uses `Option<String>` with `skip_serializing_if`. Same cosmetic difference as above.

## Enum Wire Value Verification

All enum variant wire-format strings verified against the Node.js source:

| Enum | Rust Variants | Node.js Source | Match |
|------|---------------|----------------|-------|
| `NicState` | `provisioning`, `stopped`, `running` | `VALID_NIC_STATES` in `lib/models/nic/common.js:35` | Yes |
| `BelongsToType` | `other`, `server`, `zone` | `BELONGS_TO_TYPES` in `lib/models/nic/common.js:34` | Yes |
| `LacpMode` | `off`, `active`, `passive` | `LACP_MODES` in `lib/models/aggregation.js:36` | Yes |
| `NetworkFamily` | `ipv4`, `ipv6` | `validate.enum(['ipv4', 'ipv6'])` in `lib/models/network.js:218` | Yes |
| `MorayServiceStatus` | `online`, `offline` | Literal strings in `lib/endpoints/ping.js:35,43` | Yes |
| `PingStatus` | `OK`, `initializing` | Literal strings in `lib/endpoints/ping.js:38,47` | Yes |

All enums include `#[serde(other)] Unknown` for forward compatibility.

### Missing Enum Opportunities

None identified. The `Ip.belongs_to_type` field is typed as `Option<String>` rather than `Option<BelongsToType>` -- this is intentional since the IP model may reference types beyond the NIC model's `BELONGS_TO_TYPES`.

## Response Status Code Verification

| Endpoint Pattern | Expected | Actual (Rust) | Match |
|-----------------|----------|---------------|-------|
| All POST (create) endpoints | 200 | `HttpResponseOk` (200) | Yes |
| All DELETE endpoints | 204 | `HttpResponseUpdatedNoContent` (204) | Yes |
| All GET/PUT endpoints | 200 | `HttpResponseOk` (200) | Yes |

Verified in both the Rust trait (response types) and the OpenAPI spec (status codes). All create endpoints correctly return 200 (not 201), matching Node.js `res.send(200, ...)`.

## Route Conflict Resolutions

### Path Variable Unification

Dropshot requires consistent path variable names at the same path level. Three adjustments were made:

1. **`/nic_tags/:oldname` -> `/nic_tags/{name}`**: The PUT endpoint used `:oldname` in Node.js but is unified to `{name}` to match GET/DELETE at the same path level. Functionally identical -- the service implementation treats it as the current name to look up.

2. **`/networks/:network_uuid/ips` -> `/networks/{uuid}/ips`**: Changed from `network_uuid` to `uuid` to match `/networks/{uuid}` at the same level. The `NetworkSubPath` and `IpPath` structs use `uuid` field name.

3. **`/networks/:network_uuid/nics` -> `/networks/{uuid}/nics`**: Same unification as above.

All three are wire-compatible -- clients send the same URL, only the parameter name in the OpenAPI spec differs.

## CLI Command Analysis

### Implemented Commands (42 total)

All 42 API endpoints have corresponding CLI commands organized into 8 subcommand groups plus 3 top-level commands:

- `napi ping` -- health check
- `napi nic {list,get,create,update,delete,create-on-network}` -- 6 commands
- `napi nic-tag {list,get,create,update,delete}` -- 5 commands
- `napi network {list,get,create,update,delete}` -- 5 commands
- `napi pool {list,get,create,update,delete}` -- 5 commands
- `napi ip {list,get,update}` -- 3 commands
- `napi aggregation {list,get,create,update,delete}` -- 5 commands
- `napi fabric-vlan {list,get,create,update,delete}` -- 5 commands
- `napi fabric-network {list,get,create,update,delete}` -- 5 commands
- `napi search-ips <ip>` -- IP search
- `napi gc` -- garbage collection

### Missing Commands

None.

## Field Naming Verification

All JSON field names are snake_case, matching NAPI's wire format:

- No `#[serde(rename_all = "camelCase")]` on any struct (correct)
- `heapTotal` and `heapUsed` in `MemoryUsage` have explicit `#[serde(rename)]` overrides (correct -- these come from Node.js `process.memoryUsage()`)
- All other fields use Rust's default snake_case naming, which matches the wire format

## Behavioral Notes

### Conditional Request Support (If-Match / ETag)

Node.js uses `restify.conditionalRequest()` middleware on NIC, network pool, aggregation, IP, and fabric endpoints. The Rust API trait does not explicitly model ETags -- this is a service-implementation concern, not an API-trait concern.

### Network GET Accepts "admin"

The `/networks/{uuid}` endpoint accepts the literal string "admin" as the UUID parameter. The `NetworkPath.uuid` field is typed as `String` (not `Uuid`) to accommodate this.

### IP PUT with free=true

The `UpdateIpBody` includes both `free` and `unassign` fields. When `free=true`, the service implementation should delete/unassign the IP rather than update it. This behavioral difference is documented in the struct.

### Search IPs Returns 404

`GET /search/ips` returns `ResourceNotFoundError` (404) when no results are found. This is modeled as an error return through `HttpError`, not a dual-response type.

### GC Returns 501

`GET /manage/gc` returns `NotImplementedError` (501) when GC is not exposed. Modeled as `HttpError`.

### Pagination

All list endpoints accept `limit` and `offset` query parameters. Node.js defaults: `limit=1000` (range 1-1000), `offset=0`. These defaults would be applied in the service implementation.

### Fields Projection

Fabric VLAN and Network endpoints accept a `fields` query/body parameter for response field projection. The API trait includes this parameter; implementation would filter the response accordingly.

## OpenAPI Spec Analysis

### Schema Count

35 schemas in the generated spec (all response types, request bodies, enums, and the `Error` type). No dead/unused schemas detected -- all are referenced by at least one endpoint.

### No Patched Spec Required

NAPI does not need a patched OpenAPI spec. There are no bare string responses, no empty body responses (deletes use 204 with no content), and no response format anomalies that require post-generation patching.

## Recommendations

### Low Priority

1. Consider making `NetworkPool.description` always present (not `skip_serializing_if`) to match Node.js behavior of always including the field. This would only matter for consumers doing raw JSON key-presence checks rather than typed deserialization.

2. Consider the same for `FabricVlan.name` -- always serialize, matching Node.js behavior.

3. When implementing the service, ensure the `free` field logic in IP serialize matches Node.js: `free` starts as `!reserved`, then becomes `false` if any optional param (`belongs_to_type`, `belongs_to_uuid`, `owner_uuid`) is present.

## Conclusion

**Overall Status**: READY FOR TESTING

The NAPI conversion is complete and accurate. All 34 unique handlers (42 endpoints including HEAD) are covered. All 6 enums have correct wire-format values matching the Node.js source. All response status codes are correct (200 for creates, 204 for deletes). Type definitions are complete with proper field types and optionality. The CLI provides full command coverage for all endpoints. No blocking issues were found.
