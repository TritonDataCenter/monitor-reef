<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Tritonapi Architecture: Strangler Fig Replacement for CloudAPI

## Background

Triton's public API is CloudAPI, a Node.js Restify service with 183 HTTP and
3 WebSocket endpoints
covering machines, images, networks, volumes, firewall rules, SSH keys, RBAC,
and more. Every new UI, CLI improvement, and API extension must work within or
around CloudAPI's architecture.

This document describes `tritonapi`, a Rust service that replaces CloudAPI
incrementally using the strangler fig pattern. A temporary gateway service
(`triton-gateway`) sits in front during the transition, routing requests to
tritonapi for implemented endpoints and proxying to CloudAPI for the rest.

### Goals

1. **Replace CloudAPI incrementally.** No big-bang migration. Each endpoint
   moves from CloudAPI to tritonapi independently.
2. **Modern browser authentication.** LDAP + JWT session auth for web UIs,
   alongside existing HTTP Signature auth for CLI tools.
3. **Proxy to internal APIs.** Authenticated access to VMAPI, CNAPI, NAPI,
   etc. for admin tooling.
4. **Single composite OpenAPI spec.** One spec covering everything the gateway
   exposes -- native tritonapi endpoints, proxied CloudAPI endpoints, and
   proxied internal APIs -- enabling auto-generated client libraries for web
   UIs and CLIs.
5. **Minimal, reviewable auth code.** Security-critical authentication code
   isolated in standalone libraries with comprehensive test suites.

### Non-goals

- tritonapi does not serve static assets (SPAs are deployed separately).
- tritonapi does not access databases directly (calls internal APIs instead).
- tritonapi does not terminate TLS (handled by load balancer/reverse proxy).
- triton-gateway does not terminate TLS either; a load balancer or reverse
  proxy (e.g., HAProxy, nginx) sits in front and handles TLS termination.

## Architecture

Two services, one temporary:

```
Browser SPA          CLI (triton)         Automation
(static deploy)          |                    |
     \                   |                   /
      +------------------+------------------+
                         |
             +-----------v-----------+
             |   triton-gateway      |  TEMPORARY -- dies when
             |   (Axum)              |  tritonapi is complete
             |                       |
             |   - Auth (shared libs)|
             |   - Route to tritonapi|
             |   - Proxy to CloudAPI |
             |   - /internal/* proxy |
             +-----------+-----------+
                    /    |    \
                   /     |     \
       +----------+ +---+---+ +----------+
       | tritonapi| |CloudAPI| | Internal |
       | (Drop-   | |(Node) | | APIs     |
       |  shot)   | |LEGACY | | (VMAPI,  |
       | PERMANENT| +-------+ |  CNAPI,  |
       +----------+           |  NAPI..) |
                              +----------+
```

End state (gateway removed):

```
All clients --> tritonapi (Dropshot) ---> Internal APIs
```

### Design principles

1. **tritonapi is permanent.** Every line of code in it should be written to
   last. Follows all monorepo conventions: Dropshot API traits,
   openapi-manager, Progenitor client generation.

2. **triton-gateway is throwaway scaffolding.** Exists only to make tritonapi
   useful before it is complete. No OpenAPI spec of its own. No generated
   clients. Keep it as simple as possible. Resist adding features -- every
   feature request should be answered with "implement it in tritonapi instead."

3. **Auth libraries are shared, verification is independent.** Both services
   use the same `triton-auth-verify` and JWT libraries. Both verify
   authentication independently. No trusted internal headers. tritonapi works
   correctly with or without the gateway in front of it.

### Deployment model

Same zone, two SMF-managed processes:

```
tritonapi zone:
  triton-gateway    @ 0.0.0.0:80      (public, behind LB/TLS terminator)
  triton-api-server @ 127.0.0.1:8080  (localhost only)

Config from SAPI via config-agent (shared JSON config file).
```

## Authentication

### Dual-mode auth

tritonapi will support two authentication mechanisms on the same endpoints:

- **JWT (browser clients):** User logs in via LDAP, receives a JWT access
  token and refresh token. Subsequent requests include the JWT as a Bearer
  token or HttpOnly cookie. This is new functionality that CloudAPI does not
  provide.

- **HTTP Signature (CLI clients):** Existing Triton auth mechanism. Requests
  are signed with the user's SSH private key. The `Authorization: Signature
  keyId="/{account}/keys/{fingerprint}",...` header carries the signature.
  tritonapi will verify the signature against the user's public key stored
  in UFDS.

Both mechanisms will produce the same internal caller identity (account,
UUID, roles) used by endpoint handlers.

### Shared libraries, independent verification

Security-critical auth code will live in reusable library crates:

| Library | Purpose | Used by |
|---------|---------|---------|
| `libs/triton-auth-verify` (to be created) | HTTP Signature verification, UFDS public key lookup with TTL cache | Both services |
| `libs/triton-auth-session` (to be created) | JWT creation/validation, refresh token management, LDAP authentication | Both services |
| `libs/triton-auth` (existing) | HTTP Signature signing (client-side) | Gateway (proxy signing) |

Placing JWT and LDAP code in a shared `libs/` crate (rather than duplicating
it in each service) ensures both services use identical auth logic. The LDAP
login flow is only called by tritonapi's `/auth/login` endpoint, but the JWT
verification code is called by both services.

Both services will call these libraries independently:

- The gateway will verify auth for routing decisions, logging, and proxy
  signing (JWT callers need the request re-signed with the operator key for
  CloudAPI).
- tritonapi will verify auth in each Dropshot handler to know who the caller
  is.
- Double verification is defense in depth, not duplication -- same code paths,
  negligible performance cost (JWT verification is microseconds, HTTP Signature
  with cached keys is fast).
- tritonapi will never assume the gateway has pre-verified anything. It will
  work correctly whether the gateway is in front or not.

### Auth flow through the gateway

```
JWT (browser):
  Client -> [gateway verifies JWT] -> routes to tritonapi or proxy
    If tritonapi: [tritonapi re-verifies JWT in handler]
    If CloudAPI:  [gateway signs request with operator key]

HTTP Signature (CLI):
  Client -> [gateway verifies signature] -> routes to tritonapi or proxy
    If tritonapi: [tritonapi re-verifies signature in handler]
    If CloudAPI:  [gateway passes through original Authorization header]
```

### triton-auth-verify (new library)

Server-side companion to `triton-auth` (which is client-side signing only).

Responsibilities:

- Parse `Authorization: Signature keyId="/{account}/keys/{fp}",
  algorithm="...", headers="date (request-target)", signature="..."`
- Reconstruct the signing string from `date` and `(request-target)` headers
- Look up the caller's public key from UFDS by account and fingerprint
- Cache public keys with 5-minute TTL (matching CloudAPI's current behavior)
- Verify the signature (RSA, ECDSA, Ed25519)
- Return verified caller identity (account, UUID, key fingerprint)

Design constraints:

- Standalone library with no web framework dependency
- Own comprehensive test suite, suitable for fuzz testing
- Builds incrementally: RSA first (most common), then ECDSA/Ed25519

### Dropshot auth pattern

tritonapi's Dropshot `Context` will hold shared auth infrastructure:

```rust
// Illustrative -- exact types TBD
struct ApiContext {
    auth_verifier: triton_auth_verify::Verifier, // UFDS client + key cache
    jwt_service: JwtService,                      // JWT keys + refresh store
    ldap_service: LdapService,                    // LDAP connection config
    // ... other shared state (service clients, config)
}
```

Each endpoint handler will extract and verify the caller:

```rust
async fn list_keys(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
    let caller = authenticate(&rqctx).await?; // JWT or HTTP Sig
    // ... use caller.account, caller.uuid
}
```

## tritonapi (Dropshot) -- permanent service

### Auth endpoints (new -- CloudAPI does not have these)

tritonapi's first unique value: modern browser auth that CloudAPI lacks.

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/auth/login` | LDAP authentication, returns JWT + refresh token |
| `POST` | `/auth/logout` | Revoke refresh tokens |
| `POST` | `/auth/refresh` | Rotate refresh token, get new access token |
| `GET` | `/auth/session` | Validate session, return user info |

These will be defined in the Dropshot API trait and produce an OpenAPI spec.
Web SPAs will generate TypeScript types from this spec.

### CloudAPI-compatible endpoints (incremental)

As endpoints are natively implemented, they are added to the Dropshot trait
using CloudAPI-compatible paths:

- `GET /{account}/keys`, `POST /{account}/keys`, etc.
- `GET /{account}/machines`, `POST /{account}/machines`, etc.
- Same path structure, same request/response wire format as CloudAPI

The `cloudapi-api` type definitions (already validated against real CloudAPI in
the triton-cli conversion work) are reused to ensure wire compatibility.

### Crate structure

| Crate | Status | Purpose |
|-------|--------|---------|
| `apis/triton-api` | exists (skeleton) | Dropshot API trait definition (grows over time) |
| `services/triton-api-server` | exists (skeleton) | Trait implementation, Dropshot server |
| `libs/triton-auth-verify` | to be created | HTTP Signature server-side verification |
| `libs/triton-auth-session` | to be created | JWT service, LDAP authentication |
| `clients/internal/triton-client` | to be created | Progenitor-generated client |
| `services/triton-gateway` | to be created | Temporary Axum gateway (see below) |

## triton-gateway (Axum) -- temporary scaffolding

*This service does not exist yet. It will be created as part of Phase 0.*

### Responsibilities

1. **Verify auth** -- JWT or HTTP Signature, using the same shared libraries
2. **Route to tritonapi** -- for endpoints that tritonapi implements
3. **Proxy to CloudAPI** -- for everything else, re-signing with operator key
   for JWT callers, passing through for HTTP Signature callers
4. **Proxy to internal APIs** -- under `/internal/{service}/*` for admin
   tooling

### Routing

The gateway will maintain a list of paths that tritonapi handles. Everything
else will fall through to the CloudAPI proxy. As tritonapi gains endpoints,
the list grows and the proxy handles less.

The route list should be derived from tritonapi's OpenAPI spec (read at
startup or build time) to avoid manual synchronization.

### Internal API proxy

For admin tooling (admin UI, operator scripts):

| Gateway path | Target |
|--------------|--------|
| `/internal/vmapi/*` | VMAPI |
| `/internal/cnapi/*` | CNAPI |
| `/internal/napi/*` | NAPI |
| `/internal/imgapi/*` | IMGAPI |
| `/internal/papi/*` | PAPI |
| `/internal/fwapi/*` | FWAPI |

Requires operator/admin role. Will use SAPI-based service discovery for
endpoint resolution.

Note: The internal API proxy function may outlive the CloudAPI proxy. When
the gateway is eventually retired, this capability could move into tritonapi
or become a separate lightweight service.

## OpenAPI spec composition

### Problem

The gateway exposes a unified API surface to clients:

- tritonapi's native endpoints (auth, then CloudAPI-replacement endpoints)
- Proxied CloudAPI endpoints (everything tritonapi hasn't implemented yet)
- Proxied internal APIs under `/internal/{service}/*`

Clients (web SPAs, CLIs, automation) need a single OpenAPI spec covering this
entire surface to auto-generate typed client libraries. Without composition,
clients would need to consume multiple specs and know which endpoints come
from which backend.

### Approach: build-time spec merging in openapi-manager

The `openapi-manager` tool currently supports post-generation transforms for
individual specs (see `openapi-manager/src/transforms.rs`), applying targeted
patches like error schema fixes and response format corrections. The spec
composition pipeline proposed here is **new functionality** that must be built:
reading multiple source specs, rewriting paths, merging schemas, and producing
a single composite output.

The existing transforms infrastructure provides the extension point: the
`apply_transforms` function already reads generated specs and writes patched
output. The composition logic will extend this to read additional source specs
(CloudAPI, internal APIs) and merge them.

### Composition pipeline

The pipeline reads specs that already exist in the repo and merges them:

```
apis/triton-api  ──> openapi-manager ──> triton-api.json  (native endpoints)
                                              │
                          ┌───────────────────┤
                          │                   │
                          │    Existing specs in openapi-specs/patched/:
openapi-specs/patched/    │      vmapi-api.json  ────┐
  cloudapi-api.json  ─────┤      napi-api.json   ────┐│
                          │      imgapi-api.json ───┐││
                          │      papi-api.json  ──┐ │││
                          │      sapi-api.json ─┐ │ │││
                          │                     │ │ │││
                          │    NOT YET created (need API trait crates):
                          │      cnapi-api.json     │ │ │││
                          │      fwapi-api.json     │ │ │││
                          │                     │ │ │││
                          ▼                     ▼ ▼ ▼▼▼
                    ┌─────────────────────────────────┐
                    │ NEW: openapi-manager composition │
                    │                                 │
                    │ 1. Start with triton-api.json   │
                    │ 2. Add CloudAPI paths not in    │
                    │    triton-api (proxied ones)    │
                    │ 3. Add internal API paths with  │
                    │    /internal/{svc}/ prefix      │
                    │ 4. Merge type schemas           │
                    │ 5. Add unified auth schemes     │
                    └────────────┬────────────────────┘
                                 │
                                 ▼
                    openapi-specs/patched/
                      triton-gateway-api.json  (NEW output)
                                 │
                    ┌────────────┼────────────┐
                    │            │            │
                    ▼            ▼            ▼
             TypeScript     Progenitor    Documentation
             types for      client for
             web SPAs       Rust CLIs
```

**Spec availability:** The following specs exist today and can be used as
composition inputs immediately: `cloudapi-api.json` (12,781 lines),
`vmapi-api.json` (3,924 lines), `napi-api.json` (4,019 lines),
`imgapi-api.json` (2,135 lines), `papi-api.json` (1,117 lines),
`sapi-api.json` (1,964 lines). Specs for CNAPI and FWAPI do not yet exist;
those internal APIs would need Dropshot API trait crates (following the
established `apis/*-api` pattern) before they can be included in the
composite spec. The composition pipeline should handle missing specs
gracefully -- only include internal APIs that have specs available.

### Composition rules

**CloudAPI endpoints:**

- CloudAPI paths use `/{account}/*` -- these are included as-is since
  tritonapi uses the same path convention
- Paths that already exist in the triton-api spec are excluded (those are
  served natively)
- CloudAPI schemas are merged, with triton-api schemas taking precedence on
  name conflicts
- As tritonapi implements more endpoints, fewer CloudAPI paths appear in the
  composite spec, but the total API surface remains the same

**Internal API endpoints:**

- Each internal API spec's paths are prefixed: `/vms` becomes
  `/internal/vmapi/vms`
- Parameters are preserved as-is
- Schemas are prefixed to avoid collisions: `Vm` becomes `VmapiVm`,
  `Network` becomes `NapiNetwork`, etc.
- An admin-role security requirement is added to all internal paths

**Auth scheme:**

- A unified security scheme is defined supporting both JWT Bearer and HTTP
  Signature authentication
- Applied globally to all endpoints
- Auth endpoints (`/auth/login`) are marked as public (no auth required)

**Tags:**

- Tags from all source specs are collected and merged
- Internal API tags are prefixed: `vms` becomes `vmapi:vms`

### Why this works

A key property makes the composition stable: since tritonapi's native
endpoints use the same paths and wire format as CloudAPI, the composite spec
describes the API surface correctly regardless of which backend serves a
given endpoint. Moving an endpoint from proxy to native changes the
implementation but not the spec. The composite spec only changes when:

- tritonapi adds new endpoints that CloudAPI doesn't have (auth endpoints)
- An internal API's spec changes (upstream change)
- An endpoint's types change (rare, deliberate)

### Proposed commands

These Make targets would be added to support the composition pipeline:

| Target | Description |
|--------|-------------|
| `make gateway-spec` | Generate the composite gateway spec |
| `make gateway-spec-check` | Verify composite spec is up-to-date (for CI) |

### Frontend TypeScript generation

Web SPAs would generate typed clients from the composite spec:

```bash
openapi-typescript openapi-specs/patched/triton-gateway-api.json \
    -o src/api/schema.d.ts
```

This produces TypeScript types for every endpoint the gateway exposes --
auth, machines, images, networks, internal APIs -- in a single import. SPAs
can use `openapi-fetch` with these types for fully type-safe API calls.

### Rust client generation

A `triton-gateway-client` can be generated via Progenitor from the composite
spec, providing a typed Rust client for the full gateway surface. This
could replace `cloudapi-client` for CLI tools that talk through the gateway.

Alternatively, the CLI can continue using the existing `cloudapi-client` for
CloudAPI-compatible endpoints and a separate `triton-client` (generated from
`triton-api.json`) for tritonapi-specific endpoints like auth.

## Phased roadmap

### Phase 0: tritonapi auth + gateway skeleton

**Value delivered:** Modern browser auth exists. Full CloudAPI surface
accessible through the gateway with dual-mode auth verification.

tritonapi (Dropshot):
- Auth endpoints: login, logout, refresh, session
- JWT service (LDAP-backed)
- `/ping` health check
- Auth verification via `triton-auth-verify` in handlers

triton-auth-verify (new lib):
- HTTP Signature parsing and verification
- UFDS public key lookup with TTL cache
- RSA support first, then ECDSA/Ed25519

triton-gateway (Axum):
- Dual-mode auth middleware
- Forward `/auth/*` and `/ping` to tritonapi
- Proxy everything else to CloudAPI

Composite spec:
- triton-api.json (auth + ping) merged with cloudapi-api.json

**Milestone test:**
- `POST /auth/login` with LDAP creds returns JWT
- `GET /{account}/machines` with JWT -> proxied to CloudAPI (operator-signed)
- `GET /{account}/machines` with HTTP Sig -> proxied to CloudAPI (passthrough)

### Phase 1: Kubernetes as a service

**Value delivered:** New Kubernetes on-demand functionality that goes beyond
what CloudAPI can offer.

tritonapi gains:
- Kubernetes-specific endpoints (details TBD)
- This feature currently operates via CloudAPI but will be more powerful with
  native tritonapi support, as tritonapi can implement functionality that
  does not fit within CloudAPI's existing endpoint structure

This phase demonstrates tritonapi's value as more than a CloudAPI replacement
-- it is also the home for new API capabilities.

### Phase 2: First native CloudAPI endpoints

**Value delivered:** Simple, stable endpoints served natively -- faster, no
CloudAPI dependency for these paths.

tritonapi gains:
- `GET /{account}` (account info from UFDS)
- `GET|POST|DELETE /{account}/keys` (SSH keys from UFDS)
- `GET /{account}/datacenters` (from config/SAPI)

Gateway routes these paths to tritonapi.

Composite spec: same paths, now served from tritonapi instead of CloudAPI.

**Milestone test:** `GET /{account}/keys` returns data from UFDS directly.
Response is wire-compatible with CloudAPI output (verified by integration
test).

### Phase 3: Internal API proxy + machine endpoints

**Value delivered:** Admin tooling works through authenticated proxy. Core VM
lifecycle served natively.

tritonapi gains:
- Machine CRUD + actions (start/stop/reboot) calling VMAPI via `vmapi-client`.
  The `MachineAction` endpoint uses the action-dispatch pattern documented in
  CLAUDE.md (single POST endpoint dispatching multiple operations).

Gateway gains:
- `/internal/{service}/*` proxy with SAPI discovery
- Admin role check on internal proxy routes

Composite spec: adds `/internal/vmapi/*`, `/internal/cnapi/*`, etc.

### Phase 4: Remaining endpoint groups

**Value delivered:** Majority of CloudAPI surface served natively.

Images, networks, firewall rules, volumes, packages, RBAC -- each group
calling appropriate internal APIs directly.

### Phase 5: CloudAPI replacement complete

**Value delivered:** Gateway's CloudAPI proxy removed. tritonapi is THE API.

All endpoints native. Gateway's only remaining function is the internal API
proxy. Gateway can be retired or reduced to a thin internal-API proxy if
that function is still needed.

## Security considerations

### Auth code isolation

Security-critical code will live in three places, each independently
reviewable:

1. `libs/triton-auth-verify/` -- HTTP Signature verification, standalone
2. `libs/triton-auth-session/` -- JWT service, LDAP service, standalone
3. `services/triton-gateway/src/auth/` -- Auth middleware (temporary, calls
   the shared libs above)

### UFDS public key cache

Keys cached with 5-minute TTL. Key revocation takes effect within one TTL
window. This matches CloudAPI's current behavior. If faster revocation is
needed, a cache-invalidation mechanism can be added later.

### Operator key for proxy signing

The gateway will sign proxied requests (for JWT callers) with an operator SSH
key that has full privileges. This key will be loaded from the zone's
filesystem (SAPI-configured) and never logged.

### JWT refresh token storage

In-memory initially. Refresh tokens are lost on restart (users must log in
again). For production HA with multiple instances, a shared store is needed
(UFDS attribute, persistent key-value store, or a dedicated token service).

### Path traversal protection

The gateway proxy will include percent-decoded `..` segment detection to
prevent path traversal attacks against backend services.

## Known risks

### The gateway will probably live for years

Implementing 183 CloudAPI endpoints natively is a large project. The
"throwaway" gateway will be a production service for a long time. Mitigation:
keep it genuinely simple. The gateway has no OpenAPI spec, no generated
clients, no complex business logic. Resist adding features.

### Wire-format compatibility is hard

`cloudapi-api` types handle the common cases, but CloudAPI's behavior
includes undocumented edge cases, error format quirks, and header handling
that types alone do not capture. Each native endpoint needs integration
testing that compares tritonapi's output against real CloudAPI for the same
inputs. The `conversion-plans/` methodology (which achieved comprehensive
command coverage for triton-cli) should be applied here.

### Two-crate endpoint migration

Every new tritonapi endpoint requires updating the gateway's routing. Making
the gateway derive its route list from tritonapi's OpenAPI spec (loaded at
startup) eliminates this manual synchronization.

### Refresh token persistence for HA

In-memory refresh tokens do not survive restarts and cannot be shared across
instances. This is acceptable for initial single-instance deployment but
must be addressed before multi-instance HA.

### Schema collision in composite spec

Merging multiple API specs risks type name collisions (e.g., multiple APIs
defining `Error` or `Network`). The composition pipeline must prefix or
namespace schemas from internal APIs. Portal/tritonapi schemas take
precedence over CloudAPI schemas, and CloudAPI schemas take precedence over
internal API schemas.
