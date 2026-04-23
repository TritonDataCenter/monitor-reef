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

Tracing the pipeline for a shipped endpoint. The endpoint bootstraps a
tritonapi session from an SSH-signed request: the client signs a POST
with their SSH key (account-level `/{account}/keys/{fp}` or sub-user
`/{account}/users/{user}/keys/{fp}` keyId), the server verifies the
signature against the public key mahi replicated from UFDS, and issues
a JWT pair the client uses as Bearer for subsequent calls.

### Types (no new request body)

`apis/triton-api/src/types/auth.rs` already defines `LoginResponse`
(`token`, `refresh_token`, `user: UserInfo`). `/v1/auth/login-ssh`
reuses it — the response shape is identical to the password-login path,
so downstream code doesn't care which authenticator produced the
tokens.

The endpoint has **no request body**: the signed request itself is the
proof. `Date` + `(request-target)` are the only headers covered by the
signature; anything that might confuse the signing string (content
length, content type) isn't included. This is the same
`headers="date (request-target)"` shape node-triton and
`libs/triton-auth::signature::RequestSigner` emit, so existing
HTTP-Signature clients sign the request identically to how they'd sign
any other cloudapi call.

### Endpoint declaration

`apis/triton-api/src/lib.rs` declares the endpoint on the `TritonApi`
trait:

```rust
#[endpoint {
    method = POST,
    path = "/v1/auth/login-ssh",
    tags = ["auth"],
}]
async fn auth_login_ssh(
    rqctx: RequestContext<Self::Context>,
) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError>;
```

No `TypedBody<...>` argument — the signature lives on the request as
header material the handler reads off `rqctx.request.headers()`, not as
a typed Dropshot extractor.

### Signature primitives in `libs/triton-auth`

The cross-service auth primitives live in `libs/triton-auth`, shared
between the gateway and triton-api-server:

- `auth_scheme::classify(&HeaderMap) -> AuthScheme` — one classifier
  for all services so the gateway and api-server can't disagree about
  what a request is. Variants carry their payload (`Bearer(token)`,
  `HttpSignature(params)`, `None`) so handlers skip re-parsing.
- `http_sig::parse_signature_params(&str) -> Result<ParsedSignature,
  SigError>` — tolerates the draft-cavage formatting quirks real
  signers emit (whitespace, quoted/unquoted values, escape sequences).
- `http_sig::build_signing_string(method, path_and_query, headers,
  required_headers) -> Result<String, SigError>` — fails closed when a
  required header is missing instead of treating it as empty, so an
  attacker can't truncate the signed portion of the message.
- `http_sig::verify_signature(&PublicKey, algorithm, signing_string,
  signature) -> Result<(), SigError>` — RSA (SHA-256/512, PKCS#1 v1.5),
  P-256/P-384 ECDSA (DER-encoded on the wire), Ed25519. The allowlist is
  deliberate; `rsa-sha1` and HMAC flavours are off.
- `http_sig::parse_public_key_blob(&str) -> Result<PublicKey, &str>` —
  tries OpenSSH first (`ssh-rsa AAAA...`) then falls back to PEM
  SubjectPublicKeyInfo per-algorithm (`-----BEGIN PUBLIC KEY-----...`).
  Mahi's `keys` field is deployment-specific; this helper handles both
  shapes in the wild.

The gateway uses the classifier only; the api-server uses all of these.

### Mahi lookup helpers

`libs/triton-auth-session/src/mahi.rs` has two methods:

- `MahiService::lookup(login)` — `GET /accounts?login=`. Returns
  `AuthInfo` with `account` populated. A 404 maps to
  `AuthenticationFailed` (the 401 the login handler already distinguishes
  from `MahiUnavailable`).
- `MahiService::lookup_user(account_login, user_login)` — `GET
  /users?account=&login=&fallback=false`. Returns `AuthInfo` with both
  `account` and `user` populated. `fallback=false` is critical: without
  it mahi silently returns the account-only record when the sub-user
  doesn't exist, which would let the handler succeed with unexpected
  claims.

Keys come off `auth_info.account.keys` (account form) or
`auth_info.user.keys` (sub-user form). Both are `Option<HashMap<String,
serde_json::Value>>`, keyed by fingerprint. The sub-user field had to
be promoted out of the `#[serde(flatten)] extra` catch-all on the
mahi-api `User` schema — Progenitor's generated client drops unmodeled
fields at the wire boundary (see `.claude/skills/restify-conversion/`
for the gotcha).

### The handler's seven steps

`services/triton-api-server/src/main.rs::TritonApiImpl::auth_login_ssh`
runs in a fixed order, and the comments in the source call out why each
step exists. In summary:

1. **Classify** — `auth_scheme::classify`. Reject `Bearer` with
   `WrongAuthScheme` (the whole point of this endpoint is bootstrapping
   a session from a fresh signature; a Bearer caller already has one).
   Reject `None` with the shared `unauthorized()` 401.
2. **Parse the `Authorization` value** — `http_sig::parse_signature_params`.
   Malformed input becomes `MalformedSignature` 400 via
   `sig_parse_error`.
3. **Parse the keyId** — the local `parse_key_id` function returns
   `ParsedKeyId { account, subuser: Option<String>, fingerprint }`.
   Account-level and sub-user forms are both accepted; anything else
   is `MalformedKeyId` 400.
4. **Clock-skew check** — `check_clock_skew` against the `Date` header,
   ±5 min. Explicit 400/401 codes (`MissingDateHeader`,
   `MalformedDateHeader`, `ClockSkew`) rather than letting a stale
   signature fall through to a confusing verification failure.
5. **Mahi lookup and key extraction** — branch on `subuser`. Account
   form calls `mahi.lookup(&account)`; sub-user form calls
   `mahi.lookup_user(&account, user)`. The shared `extract_public_key`
   helper pulls the blob off the right record and calls
   `http_sig::parse_public_key_blob`. **Every failure in this step
   collapses to the opaque `SignatureVerificationFailed` 401** — we
   don't let an attacker probing with arbitrary names distinguish
   "account doesn't exist" from "fingerprint doesn't exist on that
   account". The only distinct errors are the client misconfigurations
   from steps 2-4.
6. **Verify** — `http_sig::build_signing_string` +
   `http_sig::verify_signature`. Same opaque-401-on-failure story.
7. **Issue tokens** — `issue_login_response` (account form, shared tail
   with `auth_login`) or `issue_subuser_login_response` (sub-user form,
   keys the JWT on `user.uuid`/`user.login` rather than the account's).

### Gateway passthrough

`services/triton-gateway/src/main.rs` uses `auth_scheme::classify` to
branch its `/{account}/*` proxy logic: Bearer → verify the JWT and
resign with the operator SSH key before forwarding to cloudapi;
HttpSignature → pass through verbatim so unmodified cloudapi clients
(node-triton, terraform) keep working. `/v1/*` traffic is forwarded
verbatim to triton-api-server in all cases — the gateway doesn't
classify twice.

### Regeneration

Adding the endpoint + types is a trait edit, so the spec-and-client
regeneration workflow from "The pipeline" above applies. One commit
with trait changes should bundle the regenerated
`openapi-specs/generated/triton-api.json`, the merged
`openapi-specs/patched/triton-gateway-api.json`, and the regenerated
`clients/internal/triton-gateway-client/src/generated.rs`.

### CLI wiring

`cli/triton-cli/src/commands/login.rs`:

- `LoginArgs { user: Option<String> }` — `-u <login>` forces password
  login; absence triggers SSH login.
- `ssh_login(&client)` — calls `client.inner().auth_login_ssh().send()`.
  The request body is empty; the signature is supplied by the SSH
  `GatewayAuthConfig::ssh_key(...)` on the TypedClient itself.
- `password_login(&client, username)` — calls `auth_login()` with a
  `LoginRequest { username, password }` body. Prompts for the password
  interactively, or reads the undocumented `TRITON_PASSWORD` env var
  for scripted flows.
- `write_tokens` — `~/.triton/tokens/<profile>.json`, mode 0600, atomic
  temp-file-plus-rename. Deliberately outside the profile file so older
  CLIs don't trip on the fields and future Keychain/libsecret backends
  can slot in without churning the profile format.
- `load_if_fresh` / `load_or_refresh` — subsequent commands use these
  from `Cli::build_client()` to present the cached JWT as Bearer. The
  `load_or_refresh` variant transparently hits `/v1/auth/refresh` with
  the stored refresh token when the access JWT is expired.

The `Commands::Login` arm in `cli/triton-cli/src/main.rs` uses
`build_ssh_client` (not `build_client`) unconditionally, because
`/v1/auth/login-ssh` rejects Bearer by design — `Commands::Login` has
to present a fresh SSH signature even if a valid cached JWT already
exists.

### Tests

Coverage lives in three places:

- `libs/triton-auth/src/http_sig.rs` — parser / signing-string / verifier
  round-trip tests for RSA (SHA-256 and SHA-512), P-256/P-384 ECDSA,
  Ed25519, plus tamper and wrong-key regression guards. Keys are
  generated fresh per-test; no private-key fixtures are checked in.
- `libs/triton-auth/src/auth_scheme.rs` — classifier precedence tests
  (Signature beats cookie, Bearer beats cookie, empty `Signature ` is
  None, etc.). Duplicated across services before the refactor; the
  lib is now the one source of truth.
- `services/triton-api-server/src/main.rs::login_ssh_helper_tests` —
  `parse_key_id` (account form, sub-user form, SHA256 fingerprints
  containing `/`, malformed rejects) and `check_clock_skew` (window
  enforcement).

End-to-end runs against coal's headnode: see
`docs/design/tritonapi-bearer-handoff.md` for the port-forward and
redeploy cadence.

## References

- `docs/design/tritonapi-architecture.md` — how the pieces fit
- `docs/design/branch-extraction-tritonapi-skeleton.md` — current
  cleanup direction, Tier 4 / consolidation
- `docs/tutorials/api-workflow.md` — creating a new API crate
- `docs/tutorials/cli-development.md` — creating a new CLI crate
- `docs/tutorials/testing-guide.md` — test scaffolding and conventions
