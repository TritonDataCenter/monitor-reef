<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Tritonapi Architecture: Strangler Fig Replacement for CloudAPI

## Background

Triton's public API is CloudAPI, a Node.js Restify service with 183 HTTP and
3 WebSocket endpoints covering machines, images, networks, volumes, firewall
rules, SSH keys, RBAC, and more. Every new UI, CLI improvement, and API
extension must work within or around CloudAPI's architecture.

This document describes `tritonapi`, a Rust service that stands up a new
public API surface under a `/v1/` path namespace and replaces CloudAPI over
time using the strangler fig pattern. A temporary gateway service
(`triton-gateway`) sits in front during the transition, routing requests to
tritonapi for paths it owns and proxying everything else to CloudAPI.
Clients migrate off CloudAPI at their own pace by moving to `/v1/*`.

### Goals

1. **Replace CloudAPI incrementally.** No big-bang migration. Clients opt
   into `/v1/*` endpoints as tritonapi implements them; CloudAPI paths stay
   on CloudAPI until they are deprecated and removed.
2. **Modern browser authentication.** LDAP + JWT session auth for web UIs,
   alongside existing HTTP Signature auth for CLI tools. Both available on
   `/v1/*` endpoints.
3. **Composite OpenAPI spec.** One spec covering everything the gateway
   exposes -- tritonapi's native `/v1/*` endpoints and proxied CloudAPI
   endpoints -- enabling auto-generated client libraries for web UIs and
   CLIs.
4. **Minimal, reviewable auth code.** Security-critical authentication code
   isolated in standalone libraries with comprehensive test suites.

### Non-goals

- **Wire compatibility with CloudAPI.** Tritonapi's `/v1/*` endpoints use
  Rust/Dropshot defaults: snake_case fields, Dropshot's native error shape
  (including request IDs), Dropshot's pagination conventions. They are
  intentionally free to diverge from CloudAPI where a better shape exists.
  Clients that want the CloudAPI shape keep hitting the CloudAPI path.
- **Proxy to internal APIs.** The gateway does not proxy VMAPI, CNAPI,
  NAPI, etc. See "Admin UI access to internal APIs" below.
- **Static asset serving.** Tritonapi does not serve SPAs; they are deployed
  separately.
- **Direct database access.** Tritonapi calls internal APIs instead.
- **TLS termination.** Neither tritonapi nor triton-gateway terminate TLS;
  a load balancer or reverse proxy (HAProxy, nginx) sits in front.

### Admin UI access to internal APIs

The admin UI continues to access internal Triton APIs (VMAPI, CNAPI, NAPI,
IMGAPI, PAPI, FWAPI) via its existing proxy model. Moving that
responsibility to tritonapi or the gateway is out of scope for this
migration. Two possible future directions:

1. **Status quo.** Admin UI keeps its own backend-for-frontend layer.
   Tritonapi only serves public API endpoints.
2. **Tritonapi gains native admin endpoints (post-migration).** Specific
   admin operations get deliberate tritonapi endpoints under `/v1/admin/*`
   (or similar), calling internal APIs directly. Chosen case-by-case, not a
   generic proxy.

This decision is deliberately deferred until after the CloudAPI migration
completes, because coupling it to the strangler-fig timeline would conflate
two independent architectural decisions.

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
             |   (Axum)              |  CloudAPI is retired
             |                       |
             |   - Auth (shared libs)|
             |   - Route /v1/* to    |
             |     tritonapi         |
             |   - Proxy the rest    |
             |     to CloudAPI       |
             +-----------+-----------+
                    /           \
                   /             \
       +----------+             +-------+
       | tritonapi|             |CloudAPI|
       | (Drop-   |             |(Node) |
       |  shot)   |             |LEGACY |
       | PERMANENT|             +-------+
       +----------+
```

End state (gateway and CloudAPI both removed):

```
All clients --> tritonapi (Dropshot) ---> Internal APIs
```

### Design principles

1. **tritonapi is permanent.** Every line of code in it should be written to
   last. Follows all monorepo conventions: Dropshot API traits,
   openapi-manager, Progenitor client generation.

2. **triton-gateway is throwaway scaffolding with a clean death.** It exists
   to front the strangler-fig migration and dies when CloudAPI is retired.
   No OpenAPI spec of its own. No generated clients. No responsibilities
   beyond auth + routing + proxying. Keep it as simple as possible; resist
   adding features.

3. **Auth libraries are shared, verification is independent.** Both services
   use the same `triton-auth-verify` and JWT libraries. Both verify
   authentication independently. No trusted internal headers. tritonapi
   works correctly with or without the gateway in front of it.

4. **`/v1/*` is frozen once clients depend on it.** Breaking changes require
   a new path namespace (`/v2/*`), served side-by-side during its own
   migration. The strangler-fig pattern applied to tritonapi itself.

### Deployment model

Same zone, two SMF-managed processes:

```
tritonapi zone:
  triton-gateway    @ 0.0.0.0:80      (public, behind LB/TLS terminator)
  triton-api-server @ 127.0.0.1:8080  (localhost only)

Config from SAPI via config-agent (separate config files per service).
```

## Authentication

### Dual-mode auth

tritonapi supports two authentication mechanisms on the same endpoints:

- **JWT (browser clients):** User logs in via LDAP, receives a JWT access
  token and refresh token. Subsequent requests include the JWT as a Bearer
  token or HttpOnly cookie. This is new functionality that CloudAPI does not
  provide.

- **HTTP Signature (CLI clients):** Existing Triton auth mechanism. Requests
  are signed with the user's SSH private key. The `Authorization: Signature
  keyId="/{account}/keys/{fingerprint}",...` header carries the signature.
  tritonapi verifies the signature against the user's public key stored
  in UFDS.

Both mechanisms produce the same internal caller identity (account, UUID,
roles) used by endpoint handlers.

### JWT signing: ES256 (asymmetric)

Tokens are signed with ES256 (ECDSA over P-256). tritonapi holds the
private key and is the sole issuer; every verifier (triton-gateway today;
any future adminui proxy, Kubernetes gateway, or operator-portal tomorrow)
holds only the public key. This is a deliberate choice over HS256:

- **Compromise containment.** A verifier cannot forge tokens. Only a
  compromise of tritonapi itself can produce valid tokens.
- **Fan-out without coordination.** Any new DC component can accept
  tritonapi identities by obtaining the public key -- no new shared secret
  needs to be distributed, rotated in lockstep, or stored N places.
- **Public key distribution.** tritonapi exposes a JWKS document at
  `GET /v1/auth/jwks.json` (RFC 7517). Verifiers fetch it on startup and
  refresh periodically, so there is no SAPI-metadata plumbing for verifier
  keys.

The private key is generated once per zone at first boot into
`/data/jwt-private.pem` (delegated dataset, survives reprovision); the
public key lives beside it at `/data/jwt-public.pem`. Both paths are named
by the SAPI config template and read by tritonapi at startup.

### Shared libraries, independent verification

Security-critical auth code lives in reusable library crates:

| Library | Purpose | Used by |
|---------|---------|---------|
| `libs/triton-auth-verify` (to be created) | HTTP Signature verification, UFDS public key lookup with TTL cache | Both services |
| `libs/triton-auth-session` (to be created) | ES256 JWT creation/validation, refresh token management, LDAP authentication | Both services (tritonapi issues + verifies; gateway verifies only) |
| `libs/triton-auth` (existing) | HTTP Signature signing (client-side) | Gateway (proxy signing) |

Placing JWT and LDAP code in a shared `libs/` crate (rather than duplicating
it in each service) ensures both services use identical auth logic. The LDAP
login flow is only called by tritonapi's `/v1/auth/login` endpoint, but the
JWT verification code is called by both services.

Both services call these libraries independently:

- The gateway verifies auth for routing decisions, logging, and proxy
  signing (JWT callers need the request re-signed with the operator key for
  CloudAPI).
- tritonapi verifies auth in each Dropshot handler to know who the caller
  is.
- Double verification is defense in depth, not duplication -- same code
  paths, minor performance cost (JWT verification is microseconds; HTTP
  Signature with cached keys is fast, though cold-cache at both layers
  doubles UFDS load during the first hit).
- tritonapi never assumes the gateway has pre-verified anything. It works
  correctly whether the gateway is in front or not.

### Auth flow through the gateway

A **single** credential from a single login covers the whole gateway
surface. A browser that obtains a JWT from `POST /v1/auth/login` can use
that same JWT for both `/v1/*` (tritonapi-native) and `/{account}/*`
(proxied CloudAPI); the gateway handles the translation. Likewise, a CLI
that signs with an SSH key can use that same signature for both path
families. Clients do not need to track two credentials to cross the
strangler-fig boundary.

```
JWT (browser):
  Client -> [gateway verifies JWT with public key] -> routes to tritonapi or proxy
    If /v1/*:    [tritonapi re-verifies JWT with public key in handler]
    If CloudAPI: [gateway signs request with operator key]

HTTP Signature (CLI):
  Client -> [gateway verifies signature] -> routes to tritonapi or proxy
    If /v1/*:    [tritonapi re-verifies signature in handler]
    If CloudAPI: [gateway passes through original Authorization header]
```

The gateway obtains the JWT public key from tritonapi's JWKS endpoint on
startup and refreshes it periodically. No shared secret is required
between the two services.

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

tritonapi's Dropshot `Context` holds shared auth infrastructure:

```rust
// Illustrative -- exact types TBD
struct ApiContext {
    auth_verifier: triton_auth_verify::Verifier, // UFDS client + key cache
    jwt_service: JwtService,                      // JWT keys + refresh store
    ldap_service: LdapService,                    // LDAP connection config
    // ... other shared state (service clients, config)
}
```

Each endpoint handler extracts and verifies the caller:

```rust
async fn list_keys(
    rqctx: RequestContext<ApiContext>,
) -> Result<HttpResponseOk<Vec<SshKey>>, HttpError> {
    let caller = authenticate(&rqctx).await?; // JWT or HTTP Sig
    // ... use caller.account, caller.uuid
}
```

## tritonapi (Dropshot) -- permanent service

### Path and wire conventions

- **All native endpoints live under `/v1/*`.** The prefix is part of every
  `#[endpoint { path = "/v1/..." }]` declaration; Dropshot does not have a
  global-prefix feature, so convention is enforced by code review and, if
  needed, a lint.
- **Rust defaults for wire format.** snake_case field names, Dropshot's
  native error shape (`{ error_code, message, request_id }`), Dropshot
  pagination. Not CloudAPI-compatible.
- **Breaking changes bump the namespace.** `/v2/*` is added alongside
  `/v1/*` and each client migrates on its own schedule. Same strangler
  pattern applied internally.

### Auth endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/auth/login` | LDAP authentication, returns JWT + refresh token |
| `POST` | `/v1/auth/logout` | Revoke refresh tokens |
| `POST` | `/v1/auth/refresh` | Rotate refresh token, get new access token |
| `GET` | `/v1/auth/session` | Validate session, return user info |
| `GET` | `/v1/auth/jwks.json` | RFC 7517 JWKS -- public key(s) for JWT verifiers |

These are defined in the Dropshot API trait and produce an OpenAPI spec.
Web SPAs generate TypeScript types from this spec.

### Native endpoints under `/v1/*`

As endpoints are implemented, they are added to the Dropshot trait under
`/v1/*`. They are free to differ from CloudAPI's equivalents in path shape,
field naming, error format, or behavior, provided the difference serves
clarity or correctness.

- `GET /v1/account` (account info from UFDS)
- `GET|POST|DELETE /v1/keys` (SSH keys from UFDS)
- `GET /v1/datacenters` (from config/SAPI)
- Plus whatever new capabilities tritonapi adds that CloudAPI never had
  (e.g., Kubernetes-as-a-service; see Phase 1).

Clients wanting the CloudAPI wire format continue to use CloudAPI paths,
served by the gateway's proxy, until those paths are deprecated and
removed.

### Crate structure

| Crate | Status | Purpose |
|-------|--------|---------|
| `apis/triton-api` | exists (skeleton) | Dropshot API trait definition (grows over time) |
| `services/triton-api-server` | exists (skeleton) | Trait implementation, Dropshot server |
| `libs/triton-auth-verify` | to be created | HTTP Signature server-side verification |
| `libs/triton-auth-session` | to be created | JWT service, LDAP authentication |
| `clients/internal/triton-client` | to be created | Progenitor-generated client |
| `services/triton-gateway` | exists (skeleton) | Temporary Axum gateway (see below) |

## triton-gateway (Axum) -- temporary scaffolding

### Responsibilities

1. **Verify auth** -- JWT or HTTP Signature, using the same shared libraries
2. **Route to tritonapi** -- for paths tritonapi owns (`/v1/*`, `/ping`)
3. **Proxy to CloudAPI** -- for everything else, re-signing with operator
   key for JWT callers, passing through for HTTP Signature callers

That's it. No internal API proxy. No admin-role enforcement beyond what
handlers themselves do. No OpenAPI spec generation. When CloudAPI is
retired, the gateway dies.

### Routing

The gateway applies a small set of prefix rules:

```
/v1/*    → tritonapi
/ping    → tritonapi
anything → CloudAPI
else
```

The `/v1/*` rule is a clean prefix match; the gateway does not need to
enumerate individual tritonapi endpoints. This eliminates the
"synchronize-the-route-list" problem: tritonapi can add endpoints under
`/v1/*` without touching the gateway at all.

## OpenAPI spec composition

### Problem

The gateway exposes a unified API surface to clients:

- tritonapi's native `/v1/*` endpoints
- Proxied CloudAPI endpoints (everything tritonapi doesn't own)

Clients (web SPAs, CLIs, automation) benefit from a single OpenAPI spec
covering the full surface so they can auto-generate typed client libraries
from one import. Composition is not strictly required -- paths don't
collide, so SPAs could consume two specs -- but a single spec is better
ergonomics and keeps tool configuration simple.

### Approach: build-time spec merging in openapi-manager

The `openapi-manager` tool currently supports post-generation transforms
for individual specs (see `openapi-manager/src/transforms.rs`), applying
targeted patches like error schema fixes and response format corrections.
The spec composition pipeline proposed here is **new functionality** that
must be built: reading two source specs, merging schemas, and producing a
single composite output.

The existing transforms infrastructure provides the extension point: the
`apply_transforms` function already reads generated specs and writes
patched output. The composition logic extends this to read the additional
CloudAPI spec and merge it.

### Composition pipeline

```
apis/triton-api  ──> openapi-manager ──> triton-api.json  (native /v1/*)
                                              │
                          ┌───────────────────┤
                          │                   │
openapi-specs/patched/    │
  cloudapi-api.json  ─────┤
                          │
                          ▼
                    ┌─────────────────────────────────┐
                    │ NEW: openapi-manager composition│
                    │                                 │
                    │ 1. Start with triton-api.json   │
                    │ 2. Add all CloudAPI paths       │
                    │    (no overlap with /v1/*)      │
                    │ 3. Merge type schemas           │
                    │ 4. Add unified auth schemes     │
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

### Composition rules

**Paths:**

- tritonapi's `/v1/*` paths and CloudAPI's `/{account}/*` paths are
  disjoint by construction (different prefixes).
- The composite simply unions them. No path rewriting, no exclusion logic.

**Schemas:**

- Type schemas are merged. On name collision (e.g., both specs defining
  `Machine`), tritonapi's schema wins. CloudAPI's duplicate is either
  dropped or renamed (`CloudapiMachine`) -- composition pipeline decides
  per-type based on what Progenitor and openapi-typescript produce that is
  least surprising.

**Auth scheme:**

- A unified security scheme supporting both JWT Bearer and HTTP Signature
  is defined.
- Applied globally to all endpoints.
- Auth endpoints (`/v1/auth/login`) are marked as public (no auth required).

**Tags:**

- Tags from both source specs are collected and merged.

### Why this works

Because `/v1/*` and `/{account}/*` are disjoint path namespaces, the
composite spec describes a stable surface that reflects the current
deployment:

- An endpoint under `/v1/*` is served by tritonapi, with tritonapi's
  conventions.
- An endpoint under `/{account}/*` is served by CloudAPI (proxied through
  the gateway).

Moving functionality from CloudAPI to tritonapi is not "same path, new
backend" -- it is "new path, client migrates." The client-visible contract
changes only when clients opt into `/v1/*`. There is no silent migration.

### Proposed commands

| Target | Description |
|--------|-------------|
| `make gateway-spec` | Generate the composite gateway spec |
| `make gateway-spec-check` | Verify composite spec is up-to-date (for CI) |

### Frontend TypeScript generation

Web SPAs generate typed clients from the composite spec:

```bash
openapi-typescript openapi-specs/patched/triton-gateway-api.json \
    -o src/api/schema.d.ts
```

This produces TypeScript types for every endpoint the gateway exposes --
auth, native `/v1/*`, and proxied CloudAPI -- in a single import. SPAs can
use `openapi-fetch` with these types for fully type-safe API calls.

### Rust client generation

A `triton-gateway-client` can be generated via Progenitor from the
composite spec, providing a typed Rust client for the full gateway
surface.

Alternatively, the CLI can continue using `cloudapi-client` for CloudAPI
paths and a separate `triton-client` (generated from `triton-api.json`)
for native `/v1/*` paths.

## Phased roadmap

### Phase 0: tritonapi auth + gateway skeleton

**Value delivered:** Modern browser auth exists. Full CloudAPI surface
accessible through the gateway with dual-mode auth verification.

tritonapi (Dropshot):
- Auth endpoints: `/v1/auth/login`, `/v1/auth/logout`, `/v1/auth/refresh`,
  `/v1/auth/session`, `/v1/auth/jwks.json`
- JWT service (LDAP-backed, ES256)
- `/v1/ping` health check
- Auth verification via `triton-auth-verify` in handlers

triton-auth-verify (new lib):
- HTTP Signature parsing and verification
- UFDS public key lookup with TTL cache
- RSA support first, then ECDSA/Ed25519

triton-gateway (Axum):
- Dual-mode auth middleware
- Forward `/v1/*` and `/ping` to tritonapi
- Proxy everything else to CloudAPI

Composite spec:
- triton-api.json (`/v1/*` + `/ping`) merged with cloudapi-api.json

**Milestone test:**
- `POST /v1/auth/login` with LDAP creds returns JWT
- `GET /{account}/machines` with JWT → proxied to CloudAPI (operator-signed)
- `GET /{account}/machines` with HTTP Sig → proxied to CloudAPI
  (passthrough)

### Phase 1: Kubernetes as a service

**Value delivered:** New Kubernetes on-demand functionality that goes
beyond what CloudAPI can offer.

tritonapi gains:
- Kubernetes-specific endpoints under `/v1/kubernetes/*` (details TBD)
- This feature currently operates via CloudAPI but will be more powerful
  with native tritonapi support, as tritonapi can implement functionality
  that does not fit within CloudAPI's existing endpoint structure.

This phase is the motivation for the whole project: tritonapi's value is
not just replacing CloudAPI but being the home for new API capabilities
the monorepo can rapidly innovate on.

### Phase 2: First native CloudAPI-equivalent endpoint

**Value delivered:** Proof that the `/v1/*` namespace works end-to-end,
with a simple endpoint that exercises auth + UFDS access + the gateway's
routing rule.

tritonapi gains:
- `GET /v1/account` (account info from UFDS, Rust-default shape)

Gateway routes `/v1/*` to tritonapi (prefix rule, already in place).

**Milestone test:** `GET /v1/account` with JWT or HTTP Signature returns
the caller's account data in tritonapi's native shape (snake_case fields,
Dropshot error format on failure).

### Phase 3: Machine endpoints

**Value delivered:** Core VM lifecycle served natively from tritonapi.

tritonapi gains:
- Machine CRUD + actions (start/stop/reboot) under `/v1/machines` (or
  whatever path shape is cleanest) calling VMAPI via `vmapi-client`.
- `MachineAction` uses the action-dispatch pattern documented in CLAUDE.md
  if that ends up being the right shape; otherwise a more conventional
  per-action endpoint structure.

CloudAPI paths for machines continue to work (proxied) until clients
migrate.

### Phase 4: Remaining endpoint groups

**Value delivered:** Majority of CloudAPI's functional surface available
natively.

Images, networks, firewall rules, volumes, packages, RBAC -- each group
under `/v1/*`, calling the appropriate internal APIs directly.

### Phase 5: CloudAPI retirement

**Value delivered:** Gateway's CloudAPI proxy removed. tritonapi is THE
public API.

CloudAPI paths are deprecated, then removed. The gateway, whose only
remaining function was CloudAPI proxy, is retired.

## Security considerations

### Auth code isolation

Security-critical code lives in three places, each independently
reviewable:

1. `libs/triton-auth-verify/` -- HTTP Signature verification, standalone
2. `libs/triton-auth-session/` -- JWT service, LDAP service, standalone
3. `services/triton-gateway/src/auth/` -- Auth middleware (temporary,
   calls the shared libs above)

### UFDS public key cache

Keys cached with 5-minute TTL. Key revocation takes effect within one TTL
window. This matches CloudAPI's current behavior. If faster revocation is
needed, a cache-invalidation mechanism can be added later.

### Operator key for proxy signing

The gateway signs proxied requests (for JWT callers) with an operator SSH
key that has full privileges. The key is loaded from the zone's
filesystem at startup and never logged. Two SAPI config keys control it:

- `operator_key_id` -- the `keyId` value to send in the `Authorization`
  header, e.g. `/admin/keys/<fingerprint>`.
- `operator_key_file` -- absolute path to the PEM private key on the
  gateway zone, e.g. `/data/tls/operator.pem`.

Provisioning of the operator account and key pair is a one-time
bootstrap step performed when the datacenter is set up.

### JWT refresh token storage

In-memory initially, ported directly from user-portal's single-instance
model. Refresh tokens are lost on tritonapi restart (users must log in
again). A code comment at the storage site names this as the migration
point -- the obvious next move is a persistent store (UFDS attribute, a
dedicated key-value store like `moray`, or a token service) so that
restarts and multi-instance HA do not force a re-login.

### CSRF and cookie-based JWT

If JWT access tokens are delivered via HttpOnly cookie (rather than
Authorization header), CSRF protection is required. Plan: `SameSite=Strict`
on the cookie plus Origin header validation on state-changing requests.
Double-submit tokens if SameSite turns out to be insufficient.

### Path traversal protection

The gateway proxy includes percent-decoded `..` segment detection to
prevent path traversal attacks against backend services.

## Known risks

### Wire-format compatibility is NOT a goal

Earlier drafts of this doc promised that tritonapi's native endpoints
would be wire-compatible with CloudAPI's. That bet has been retracted.
Tritonapi's `/v1/*` endpoints use Rust defaults and are free to diverge.
The migration story is now "clients opt into `/v1/*` when ready," not
"endpoint switches backend transparently." This removes a large class of
subtle-bug risk but makes client migration an explicit per-client effort.

### Gateway dies cleanly, eventually

The gateway's lifetime is bounded by CloudAPI's. As long as CloudAPI
exists, the gateway exists. When CloudAPI is retired, the gateway is
retired with it. Keep the gateway simple so this retirement is
straightforward.

### Two-crate endpoint migration is minimal

The `/v1/*` prefix rule means the gateway does not need to enumerate
tritonapi endpoints; it just matches the prefix. Adding a new tritonapi
endpoint is a one-crate change (apis/triton-api + services/triton-api-server).

### Refresh token persistence for HA

In-memory refresh tokens do not survive restarts and cannot be shared
across instances. This is acceptable for initial single-instance
deployment but must be addressed before multi-instance HA.

### Schema collision in composite spec

Merging tritonapi's spec with CloudAPI's risks type-name collisions (e.g.,
both define `Machine` or `Network`). The composition pipeline must pick a
precedence rule (tritonapi wins) and decide whether colliding CloudAPI
types are renamed or dropped. Simpler than the earlier multi-spec design
but still not zero work.
