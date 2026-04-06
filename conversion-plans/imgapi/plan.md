# IMGAPI Conversion Plan

## Source
- Path: `./target/sdc-imgapi`
- Version: 4.13.1
- Package name: imgapi

## Endpoints Summary
- Total: 28 (excluding redirects/static docs)
- By method: GET: 8, POST: 13, PUT: 3, DELETE: 3
- Source files: `lib/app.js`, `lib/images.js`, `lib/channels.js`, `lib/datasets.js`, `lib/authkeys.js`

Note: `lib/docker.js` and `lib/lxd.js` contain handler implementations called from `lib/images.js` but do not define routes themselves.

## Endpoints Detail

### Miscellaneous (from lib/app.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /ping | apiPing | Returns ping object; `?error=<name>` triggers error for testing |
| GET | /state | AdminGetState | Admin-only; returns state snapshot object |
| POST | /state?action=dropcaches | AdminUpdateState | Admin-only; returns 202 |

### Channels (from lib/channels.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /channels | apiListChannels | Returns array of channel objects; only mounted if channels configured |

### Images - Core CRUD (from lib/images.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /images | apiListImages | Returns array of serialized images |
| GET | /images/:uuid | apiGetImage | Returns single serialized image; uses ETag/conditional request |
| POST | /images | apiCreateImage | No action param; creates new image from body manifest |
| POST | /images/:uuid | UpdateImage (action dispatch) | See action dispatch section below |
| DELETE | /images/:uuid | apiDeleteImage | Returns 204 no content |

### Images - File Management (from lib/images.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| PUT | /images/:uuid/file | apiAddImageFile / apiAddImageFileFromSource | Raw body upload; `?source=` variant fetches from remote IMGAPI |
| POST | /images/:uuid/file/from-url | apiAddImageFileFromUrl | Body: `{ file_url: "https://..." }` |
| GET | /images/:uuid/file | apiGetImageFile | Returns raw file stream (binary) |

### Images - Icon Management (from lib/images.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| PUT | /images/:uuid/icon | apiAddImageIcon | Raw body upload; content-type must be image/* |
| GET | /images/:uuid/icon | apiGetImageIcon | Returns raw icon stream (binary) |
| DELETE | /images/:uuid/icon | apiDeleteImageIcon | Returns serialized image |

### Images - ACL Management (from lib/images.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /images/:uuid/acl?action=add | apiAddImageAcl | Body is JSON array of UUIDs |
| POST | /images/:uuid/acl?action=remove | apiRemoveImageAcl | Body is JSON array of UUIDs |

### Images - Jobs (from lib/images.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /images/:uuid/jobs | apiListImageJobs | Returns array of jobs from wfapi |

### Images - Clone (from lib/images.js, dc mode only)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /images/:uuid/clone | apiCloneImage | `?account=<uuid>` required; returns serialized image |

### Images - Admin Push (from lib/images.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /images/:uuid/push | apiAdminPushDockerImage | Docker-only; streams JSON progress messages |

### Auth Keys (from lib/authkeys.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| POST | /authkeys/reload | apiAdminReloadAuthKeys | Admin-only; returns `{}` |

### Datasets (legacy redirects, from lib/datasets.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /datasets | redir('/images') | Legacy redirect |
| GET | /datasets/:arg | apiGetDataset | Legacy redirect to /images/* based on URN parsing |

### Static/Utility (from lib/app.js)

| Method | Path | Handler | Notes |
|--------|------|---------|-------|
| GET | /favicon.ico | apiFavicon | Static file |
| GET | / | redir('/docs/') | Redirect |
| GET | /docs | redir('/docs/') | Redirect |
| GET | /docs/(.*) | serveStatic | Static docs files |


## Route Conflicts

No route conflicts found. All parameterized routes use unique path prefixes:
- `/images/:uuid` vs `/images/:uuid/file` vs `/images/:uuid/icon` etc. -- all distinct due to additional path segments
- `/datasets/:arg` -- separate from `/images` prefix entirely
- No literal-vs-parameterized conflicts at the same path level

**Status: RESOLVED** -- no conflicts.


## Action Dispatch Endpoints

### POST /images/:uuid (UpdateImage action dispatch)

This endpoint dispatches to multiple handlers via `req.query.action`. The handlers are chained in Restify middleware order -- each checks the action value and calls `next()` to pass to the next handler if it doesn't match.

| Action | Handler | Required Fields | Optional Fields | Response | Notes |
|--------|---------|-----------------|-----------------|----------|-------|
| import | apiAdminImportImage | body: full image manifest with uuid matching URL | query: skip_owner_check, channel | Image | Admin-only; no `?source` |
| import (with source) | apiAdminImportImageFromSource | query: source | query: skip_owner_check, storage, channel | Image | Admin-only; fetches manifest from remote |
| import-remote | apiAdminImportRemoteImage | query: source | query: skip_owner_check | `{ image_uuid, job_uuid }` | Admin-only; creates workflow job |
| import-from-datacenter | apiImportImageFromDatacenter | query: datacenter, account | (none) | `{ image_uuid, job_uuid }` | Creates workflow job |
| import-docker-image | apiAdminImportDockerImage | query: repo, tag OR digest | query: public, headers: x-registry-auth, x-registry-config | Streaming JSON messages | Admin-only; delegates to docker.js |
| import-lxd-image | apiAdminImportLxdImage | (see lxd.js) | (see lxd.js) | (streaming) | Admin-only; delegates to lxd.js |
| change-stor | apiAdminChangeImageStor | query: stor | (none) | Image (with admin fields) | Admin-only |
| export | apiExportImage | query: manta_path | query: account | `{ manta_url, image_path, manifest_path }` | |
| activate | apiActivateImage | (none) | (none) | Image | |
| enable | apiEnableDisableImage | (none) | (none) | Image | |
| disable | apiEnableDisableImage | (none) | (none) | Image | |
| channel-add | apiChannelAddImage | body: channel (name) | (none) | Image | Only if channels configured |
| update | apiUpdateImage | body: mutable fields | (varies) | Image | See imgmanifest for mutable fields |

**Note on CreateImage:** `POST /images` also dispatches:
- No action param: `apiCreateImage` -- creates image from manifest body
- `action=create-from-vm`: `apiCreateImageFromVm` -- query: vm_uuid, incremental, max_origin_depth, account; returns `{ image_uuid, job_uuid }`
- `action=import-docker-image`: `apiAdminImportDockerImage` (streaming)
- `action=import-lxd-image`: `apiAdminImportLxdImage` (streaming)

### POST /images/:uuid/acl (ACL action dispatch)

| Action | Handler | Required Fields | Response | Notes |
|--------|---------|-----------------|----------|-------|
| add (or no action) | apiAddImageAcl | body: array of UUIDs | Image | Default if no action specified |
| remove | apiRemoveImageAcl | body: array of UUIDs | Image | |

### POST /state (State action dispatch)

| Action | Handler | Response | Notes |
|--------|---------|----------|-------|
| dropcaches | apiDropCaches | 202 (no body) | Admin-only |


## Planned File Structure

```
apis/imgapi-api/src/
  lib.rs          # API trait definition with all endpoints
  types/
    mod.rs        # Re-exports
    common.rs     # Uuid alias, shared types
    image.rs      # Image manifest struct, ImageState, ImageType, ImageOs, etc.
    file.rs       # ImageFile, FileCompression
    channel.rs    # Channel struct
    ping.rs       # PingResponse
    state.rs      # AdminState (opaque)
    job.rs        # Job-related types
    action.rs     # Action enums for dispatch endpoints
```


## Enum Opportunities

### ImageState
- Field: `state` on Image manifest
- Variants: `active`, `unactivated`, `disabled`, `creating`, `failed`
- Needs `#[serde(other)] Unknown`: Yes (server-controlled state)

### ImageType
- Field: `type` on Image manifest
- Variants: `zone-dataset`, `lx-dataset`, `zvol`, `docker`, `lxd`, `other`
- Needs `#[serde(rename)]` on each variant for hyphenated names
- Note: `type` is a Rust keyword, field must use `#[serde(rename = "type")]`
- Special: code treats `"null"` as absent (see `if (this.type === 'null') delete data.type`)

### ImageOs
- Field: `os` on Image manifest
- Variants: `smartos`, `linux`, `windows`, `bsd`, `illumos`, `other`
- Special: code treats `"null"` as absent (see `if (this.os === 'null') delete data.os`)

### FileCompression
- Field: `compression` on image file objects
- Variants: `gzip`, `bzip2`, `xz`, `none`
- Used in: AddImageFile, AddImageFileFromUrl, AddImageFileFromSource query params

### StorageType
- Field: `stor` on image file objects (admin-only)
- Variants: `local`, `manta`
- Also used in `storage` query param and `storageTypes` in state snapshot

### ImageAction (for POST /images/:uuid)
- Variants: `import`, `import-remote`, `import-from-datacenter`, `import-docker-image`, `import-lxd-image`, `change-stor`, `export`, `activate`, `enable`, `disable`, `channel-add`, `update`
- All use hyphenated wire format

### CreateImageAction (for POST /images)
- Variants: `create-from-vm`, `import-docker-image`, `import-lxd-image`, `import-from-datacenter`

### AclAction (for POST /images/:uuid/acl)
- Variants: `add`, `remove`

### StateAction (for POST /state)
- Variants: `dropcaches`


## Patch Requirements

### Streaming endpoints (cannot be represented in standard OpenAPI)
- `POST /images?action=import-docker-image` -- streams JSON progress messages via `res.write()` + `res.end()`
- `POST /images?action=import-lxd-image` -- same streaming pattern
- `POST /images/:uuid/push` -- streams JSON progress messages
- **Recommendation**: These endpoints need custom handling. Either exclude from the Dropshot trait and implement separately, or use a channel/WebSocket approach.

### Binary upload/download endpoints
- `PUT /images/:uuid/file` -- raw binary body upload (not JSON)
- `GET /images/:uuid/file` -- raw binary stream response
- `PUT /images/:uuid/icon` -- raw binary body upload
- `GET /images/:uuid/icon` -- raw binary stream response
- **Recommendation**: Use Dropshot's `UntypedBody` for uploads and raw `Response` for downloads.

### Non-200 responses
- `DELETE /images/:uuid` -- returns 204 no content (`HttpResponseDeleted`)
- `POST /state?action=dropcaches` -- returns 202 (`HttpResponseAccepted<()>`)
- `POST /authkeys/reload` -- returns 200 with `{}` (empty object)

### Variable response types on same path
- `POST /images/:uuid` -- different actions return different response shapes:
  - Most actions return serialized `Image`
  - `import-remote`, `import-from-datacenter` return `{ image_uuid, job_uuid }`
  - `export` returns `{ manta_url, image_path, manifest_path }`
  - `import-docker-image`, `import-lxd-image` are streaming
  - `change-stor` returns `Image` with admin fields
- **Recommendation**: Split into separate Dropshot endpoints per action (Dropshot cannot multiplex response types). The `?action=` dispatch pattern maps to separate trait methods.

### Conditional fields in Image serialization
- Some fields are conditionally included (e.g., `billing_tags` and `traits` only in non-public mode, `channels` only for Accept-Version >= 2.0.0, `pid` and `user` on ping response only for authenticated requests)
- **Recommendation**: Use `Option<T>` for all conditional fields. The `Accept-Version`-gated `channels` field should always be present in the new API.

### ETag / conditional request support
- `GetImage`, `GetImageFile`, `GetImageIcon` use ETags
- Dropshot does not have built-in conditional request support
- **Recommendation**: Implement ETag headers manually in the service layer.


## Types to Define

### Image (response)
Core fields (always present):
- `v`: u32
- `uuid`: Uuid
- `owner`: Uuid
- `name`: String
- `version`: String
- `state`: ImageState
- `disabled`: bool
- `public`: bool
- `published_at`: Option<String> (ISO date; present if activated)
- `type`: Option<ImageType> (absent if was "null")
- `os`: Option<ImageOs> (absent if was "null")
- `files`: Vec<ImageFile>

Optional fields:
- `acl`: Option<Vec<Uuid>>
- `description`: Option<String>
- `homepage`: Option<String>
- `eula`: Option<String>
- `icon`: Option<bool> (true if icon exists)
- `urn`: Option<String> (legacy)
- `requirements`: Option<serde_json::Value> (nested object)
- `users`: Option<Vec<ImageUser>>
- `generate_passwords`: Option<bool>
- `inherited_directories`: Option<Vec<String>>
- `origin`: Option<Uuid>
- `nic_driver`: Option<String> (zvol only)
- `disk_driver`: Option<String> (zvol only)
- `cpu_type`: Option<String> (zvol only)
- `image_size`: Option<u64> (zvol only)
- `tags`: Option<serde_json::Value> (key/value object)
- `billing_tags`: Option<Vec<String>> (non-public mode only)
- `traits`: Option<serde_json::Value> (non-public mode only)
- `error`: Option<ImageError> (only when state=failed)
- `channels`: Option<Vec<String>>

### ImageFile
- `sha1`: String
- `size`: u64
- `compression`: FileCompression
- `dataset_guid`: Option<String>
- `stor`: Option<StorageType> (admin-only)
- `digest`: Option<String> (docker-only)
- `uncompressedDigest`: Option<String> (docker-only, deprecated)

### ImageUser
- `name`: String

### ImageError
- `message`: Option<String>
- `code`: Option<String>
- `url`: Option<String>

### ImageRequirements
- `networks`: Option<Vec<NetworkRequirement>>
- `brand`: Option<String>
- `ssh_key`: Option<bool>
- `min_ram`: Option<u64>
- `max_ram`: Option<u64>
- `min_platform`: Option<HashMap<String, String>>
- `max_platform`: Option<HashMap<String, String>>
- `bootrom`: Option<String>

### NetworkRequirement
- `name`: String
- `description`: Option<String>

### Channel
- `name`: String
- `description`: String
- `default`: Option<bool>

### PingResponse
- `ping`: String ("pong")
- `version`: String
- `imgapi`: bool
- `pid`: Option<u64> (only for authenticated/dc requests)
- `user`: Option<String> (only for authenticated requests)

### CreateImageFromVmResponse
- `image_uuid`: Uuid
- `job_uuid`: Uuid

### ExportImageResponse
- `manta_url`: String
- `image_path`: String
- `manifest_path`: String

### AdminStateSnapshot
- Opaque object (`serde_json::Value`) -- internal structure varies

### CreateImageRequest (body for POST /images)
All Image fields that are settable on create (see Image.create in images.js):
- `v`: Option<u32>
- `owner`: Uuid
- `name`: String
- `version`: String
- `type`: ImageType
- `os`: ImageOs
- `public`: Option<bool>
- `disabled`: Option<bool>
- `acl`: Option<Vec<Uuid>>
- `description`: Option<String>
- `homepage`: Option<String>
- `eula`: Option<String>
- `icon`: Option<bool>
- `error`: Option<ImageError>
- `requirements`: Option<serde_json::Value>
- `users`: Option<Vec<ImageUser>>
- `traits`: Option<serde_json::Value>
- `tags`: Option<serde_json::Value>
- `billing_tags`: Option<Vec<String>>
- `generate_passwords`: Option<bool>
- `inherited_directories`: Option<Vec<String>>
- `origin`: Option<Uuid>
- `channels`: Option<Vec<String>>
- For zvol type: `nic_driver`, `disk_driver`, `cpu_type`, `image_size`


## Field Naming Exceptions

All image manifest fields use **snake_case** in the wire format. This is different from CloudAPI which uses camelCase. IMGAPI consistently uses snake_case throughout.

Key snake_case fields:
- `published_at`
- `generate_passwords`
- `inherited_directories`
- `nic_driver`
- `disk_driver`
- `cpu_type`
- `image_size`
- `billing_tags`
- `dataset_guid`
- `image_uuid`
- `job_uuid`
- `manta_url`
- `image_path`
- `manifest_path`
- `manta_path`
- `vm_uuid`
- `max_origin_depth`
- `skip_owner_check`
- `file_url`

**Recommendation**: Use `#[serde(rename_all = "snake_case")]` on all IMGAPI types (or simply use snake_case Rust field names with no rename needed, since Rust convention already matches).

Note: The `uncompressedDigest` field on `ImageFile` is **camelCase** -- this is the one exception. Use `#[serde(rename = "uncompressedDigest")]`.


## WebSocket/Channel Endpoints

None found. IMGAPI does not use WebSocket or Server-Sent Events.

However, three endpoints use **streaming JSON** responses (newline-delimited JSON via `res.write()`):
- `POST /images?action=import-docker-image`
- `POST /images?action=import-lxd-image`
- `POST /images/:uuid/push`

These are not true WebSocket endpoints but will need special handling in Dropshot (possibly via channel endpoints or custom response types).


## Additional Notes

### Mode-dependent behavior
IMGAPI runs in different modes: `dc` (datacenter), `public`, `private`. Some endpoints and behaviors are mode-dependent:
- `CloneImage` only available in `dc` mode
- `billing_tags` and `traits` hidden in `public` mode
- Owner handling differs between modes
- Auth requirements differ

### Legacy dataset endpoints
The `/datasets` endpoints are pure redirects to `/images` for backward compatibility with DSAPI. Consider whether to include these in the Rust API or handle them as separate redirect middleware.

### Workflow (wfapi) integration
Several endpoints create workflow jobs and return `{ image_uuid, job_uuid }`. The workflow API integration is a separate concern from the IMGAPI itself.

### Authentication patterns
- `reqAuth` (strict): Required for all mutating operations
- `reqPassiveAuth` (passive): Pass-through if no Authorization header; enforced if present
- Auth type configurable: `none`, `signature`


## Phase 2 Complete

- API crate: `apis/imgapi-api/`
- OpenAPI spec: `openapi-specs/generated/imgapi-api.json`
- Endpoint count: 22 (consolidated from 28 -- excludes static/favicon/docs routes, merges action dispatch into single endpoints)
- Enums: 9 (ImageState, ImageType, ImageOs, FileCompression, StorageType, ImageAction, CreateImageAction, AclAction, StateAction)
- Action request types: 12 for POST /images/:uuid actions + CreateImageFromVmRequest for POST /images
- Binary endpoints: UntypedBody for file/icon upload, Response<Body> for download
- Streaming endpoints: Response<Body> for docker/lxd import and push
- Legacy dataset redirects: included as Response<Body> endpoints
- Build status: SUCCESS (no warnings)
- OpenAPI check: PASS

### Endpoint mapping

| Trait Method | Original Endpoint | Response Type |
|-------------|-------------------|---------------|
| `ping` | GET /ping | `HttpResponseOk<PingResponse>` |
| `admin_get_state` | GET /state | `HttpResponseOk<serde_json::Value>` |
| `admin_update_state` | POST /state?action=dropcaches | `HttpResponseAccepted<()>` |
| `list_channels` | GET /channels | `HttpResponseOk<Vec<Channel>>` |
| `list_images` | GET /images | `HttpResponseOk<Vec<Image>>` |
| `get_image` | GET /images/:uuid | `HttpResponseOk<Image>` |
| `create_image` | POST /images (action dispatch) | `Response<Body>` |
| `image_action` | POST /images/:uuid (action dispatch) | `Response<Body>` |
| `delete_image` | DELETE /images/:uuid | `HttpResponseDeleted` |
| `add_image_file` | PUT /images/:uuid/file | `HttpResponseOk<Image>` |
| `add_image_file_from_url` | POST /images/:uuid/file/from-url | `HttpResponseOk<Image>` |
| `get_image_file` | GET /images/:uuid/file | `Response<Body>` |
| `add_image_icon` | PUT /images/:uuid/icon | `HttpResponseOk<Image>` |
| `get_image_icon` | GET /images/:uuid/icon | `Response<Body>` |
| `delete_image_icon` | DELETE /images/:uuid/icon | `HttpResponseOk<Image>` |
| `image_acl_action` | POST /images/:uuid/acl | `HttpResponseOk<Image>` |
| `list_image_jobs` | GET /images/:uuid/jobs | `HttpResponseOk<Vec<Value>>` |
| `clone_image` | POST /images/:uuid/clone | `HttpResponseOk<Image>` |
| `admin_push_image` | POST /images/:uuid/push | `Response<Body>` |
| `admin_reload_auth_keys` | POST /authkeys/reload | `HttpResponseOk<Value>` |
| `list_datasets` | GET /datasets | `Response<Body>` |
| `get_dataset` | GET /datasets/:arg | `Response<Body>` |


## Phase 3 Complete

- Client crate: `clients/internal/imgapi-client/`
- Build status: SUCCESS
- Typed wrappers: YES (TypedClient with ActionError)
  - POST /images/:uuid actions: import_image, import_remote_image, import_from_datacenter, change_stor, export_image, activate_image, enable_image, disable_image, channel_add_image, update_image
  - POST /images actions: create_image_from_manifest, create_image_from_vm
  - POST /images/:uuid/acl actions: add_image_acl, remove_image_acl
  - POST /state actions: drop_caches
  - Streaming actions (import-docker-image, import-lxd-image, push) left as raw ByteStream via inner()
- ValueEnum patches: ImageState, ImageType, ImageOs, FileCompression, StorageType
- Re-exports: 50 types from imgapi-api
- Dependencies: bytes, futures-util (for ByteStream consumption), clap, schemars (for generated code)

### ByteStream handling

IMGAPI is the first client with `Response<Body>` endpoints (action dispatch returns variable response shapes). Progenitor maps these to `ByteStream`. The TypedClient collects the stream into bytes using `futures_util::TryStreamExt` and deserializes to the expected type for each action. Streaming endpoints (Docker/LXD import, push) are left as raw ByteStream for callers to consume incrementally.


## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [x] Phase 3: Generate Client - COMPLETE
- [ ] Phase 4: Generate CLI
- [ ] Phase 5: Validate
