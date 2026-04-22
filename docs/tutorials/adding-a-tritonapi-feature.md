<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Adding a feature to `triton-api` (and surfacing it in `triton-cli`)

How to extend the tritonapi surface end-to-end: define a new `/v1/*`
endpoint on `triton-api-server`, flow it through the spec/client
pipeline, and call it from `triton-cli`.

This is about adding *to an existing* API. For bootstrapping a brand-new
API crate from scratch, see `docs/tutorials/api-workflow.md`. For
building a new CLI on top of a generated client, see
`docs/tutorials/cli-development.md`. The specifics here — how
`triton-api`, the gateway, `triton-gateway-client`, and `triton-cli`
compose — live in `docs/design/tritonapi-architecture.md`.

## When to use this guide

Use it when you're adding:
- A new endpoint under `/v1/*` (auth, sessions, an admin action, a new
  tritonapi-native resource).
- A new field on an existing endpoint's request or response.
- A new CLI command that consumes one of the above.

Skip it when you're adding a `/{account}/*` cloudapi endpoint — those
live in the Node.js cloudapi service, not `triton-api-server`. The Rust
gateway just proxies them through.

## The pipeline

The chain from "write Rust" to "triton CLI calls it" runs through five
artifacts, in this order:

1. **API trait** — `apis/triton-api/src/lib.rs` + types under
   `apis/triton-api/src/types/`. Declares the wire contract (paths,
   methods, request/response types, tags).
2. **Server implementation** — `services/triton-api-server/src/main.rs`.
   The `impl TritonApi for ApiContext` block. All context plumbing
   (JWT, mahi, LDAP) is already wired; new endpoints pull what they need
   from `rqctx.context()`.
3. **Generated OpenAPI spec** — `openapi-specs/generated/triton-api.json`.
   Produced by `make openapi-generate` from the API trait. Checked into
   git. `make openapi-check` enforces it matches the trait in CI.
4. **Merged gateway spec** — `openapi-specs/patched/triton-gateway-api.json`.
   Produced by the same `make openapi-generate` run. Contains both the
   cloudapi `/{account}/*` paths and the tritonapi `/v1/*` paths in one
   document, for a single client to consume.
5. **Generated client** — `clients/internal/triton-gateway-client/src/generated.rs`.
   Produced by `make clients-generate` from the merged spec. Checked in.
   `make clients-check` enforces freshness.

Everything downstream of the CLI (`cli/triton-cli/src/`) imports from
`triton-gateway-client` and calls `client.inner().<new_endpoint>()`
directly — no dispatch macros, no wrapper ceremony, because there's a
single Progenitor-generated client in the CLI.

### Regeneration workflow

After editing the API trait or types:

```
make openapi-generate   # rebuild generated + patched specs
make clients-generate   # rebuild client generated.rs
make openapi-check      # verify specs match traits (CI check)
make clients-check      # verify generated client matches spec (CI check)
```

Commit the regenerated files alongside the trait edits — they're part
of the public contract of the change.

## Per-feature decisions

Before writing code, pick answers for each of these. They determine
which parts of the pipeline you need to touch and how the endpoint
composes with the rest of the system.

### 1. Endpoint shape

- **Path**: what URL does the client call? `/v1/<resource>/<id>`,
  `/v1/<resource>/<action>`, `/v1/<action>`? Look at
  `apis/triton-api/src/lib.rs` for existing patterns (`/v1/auth/login`,
  `/v1/ping`, `/v1/session`).
- **Method**: GET for reads, POST for side-effectful actions or
  resource creation. Prefer POST with a body over GET with many query
  params when the request is non-trivial.
- **Path params vs body**: IDs go in the path; complex inputs go in the
  body; filters/pagination go in query params.

### 2. Auth policy

Every `/v1/*` endpoint declares its auth expectations in the
implementation. The triton-gateway's `auth_scheme` classifier
(`services/triton-gateway/src/main.rs`) groups traffic into Bearer /
HTTP-Signature / None; which of those an endpoint accepts is the
endpoint's own call.

Conventions:
- **Public** (no auth required): health/readiness probes (`/v1/ping`),
  JWKS (`/v1/auth/jwks.json`). Handler doesn't call the JWT verifier.
- **Authenticated — Bearer or HTTP-Signature (default)**: anything that
  acts on behalf of an authenticated user. Accept both credential
  types; the client picks which to present. The handler inspects the
  `Authorization` header: `Bearer …` → verify the JWT via the existing
  `jwks.verify_token` path; `Signature …` → parse the keyId, resolve
  the key via mahi, verify the signature. Factor the classifier into
  a shared helper so handlers don't each reimplement it.
- **Narrower policy** (Bearer-only, Signature-only, or a specific
  subset) requires justification. Typical reasons: the endpoint
  operates on a specific token (`/v1/auth/logout` revokes a JTI from
  the presented Bearer); the endpoint is a bootstrap that only makes
  sense with a fresh signature (`/v1/auth/login-ssh`). Default to
  accepting both and narrow only when a concrete reason forces it — if
  a client ever has trouble with one scheme, we'd rather tell them to
  switch than drop server-side support.

### 3. Mahi vs. UFDS for lookups

The triton-api-server has both available. Rules of thumb:
- **mahi** for auth hot paths: account lookup, group membership, SSH
  key lookup by fingerprint. Redis-backed, typically <1ms. All
  `/v1/auth/*` verification should use mahi.
- **UFDS (LDAP)** for writes and for auth primitives mahi doesn't cache:
  password verification (`/v1/auth/login` today), key registration,
  user create/update. Slower; acceptable on non-hot paths.

### 4. Error shape

Errors leaving the tritonapi surface follow the shape defined by the
auto-generated `Error` schema in `openapi-specs/generated/triton-api.json`:
`{error_code?, message, request_id}`. Dropshot produces this
automatically from `HttpError`. Use `session_error_to_http` (see
`services/triton-api-server/src/main.rs`) or `HttpError::for_*`
constructors — don't hand-roll the body.

Cloudapi errors on `/{account}/*` use a different shape (`{code,
message?, request_id?}`) and pass through the gateway verbatim. Don't
try to unify them at the gateway layer (Phase 0 attempted this and was
reverted — see `docs/design/branch-extraction-tritonapi-skeleton.md`).

### 5. CLI command shape

In `cli/triton-cli/`:
- **Top-level command** (`triton <verb>`) for things the user does
  frequently. Listed in the main `Commands` enum in
  `cli/triton-cli/src/main.rs`.
- **Subcommand** under an existing grouping for related operations.
- Output: human-readable table by default, `-j` for JSON. Follow the
  patterns in `cli/triton-cli/src/output/`.
- Client access: `client.inner().<generated_method>()` — the single
  `triton_gateway_client::TypedClient` the CLI owns.

## Profile and token interactions

The CLI's profile format is in `cli/triton-cli/src/config/profile.rs`.
It's intentionally forward-compatible: no `deny_unknown_fields`, so
older CLIs reading a newer profile silently ignore unknown fields.
Adding optional fields is safe without a version bump.

Expected additions as the Bearer flow comes back:
- A (future) token cache — **stored outside the profile file**, probably
  at `~/.triton/tokens/<profile>.json` with mode 0600 and atomic write,
  behind a pluggable storage trait so Mac Keychain / libsecret / etc.
  can back-end it later.
- The profile may or may not have a `keyId` — if it doesn't, the login
  command should fall back to prompting for a password (using the
  existing `POST /v1/auth/login`).

### `triton login` decision tree (sketch)

```
profile has keyId?
├── yes → sign HTTP-Sig request to /v1/auth/login-ssh, stash returned JWT.
└── no  → prompt for password, POST to /v1/auth/login, stash returned JWT.
```

Either path produces the same `LoginResponse`; the token cache doesn't
care which auth produced it.

## Worked example: `POST /v1/auth/login-ssh`

*To be filled in as the feature lands.* Will cover:
- New `LoginSshRequest` / reuse of `LoginResponse` in
  `apis/triton-api/src/types/auth.rs`.
- Endpoint declaration in `apis/triton-api/src/lib.rs` alongside the
  existing `auth_login`.
- Signature-verification helper in `triton-auth-session` (or a new
  crate), mahi key lookup, JWT creation.
- Regeneration commits for `openapi-specs/generated/triton-api.json`
  and `triton-gateway-api.json` + `triton-gateway-client/src/generated.rs`.
- `triton login` CLI command wiring.
- Integration tests: unit test for the signature verifier, end-to-end
  test against a stub triton-api-server with a stub mahi.

Until this section is populated, read the existing `auth_login`
implementation (`services/triton-api-server/src/main.rs`) as the
closest prior art.

## References

- `docs/design/tritonapi-architecture.md` — how the pieces fit
- `docs/design/branch-extraction-tritonapi-skeleton.md` — current
  cleanup direction, Tier 4 / consolidation
- `docs/tutorials/api-workflow.md` — creating a new API crate
- `docs/tutorials/cli-development.md` — creating a new CLI crate
- `docs/tutorials/testing-guide.md` — test scaffolding and conventions
