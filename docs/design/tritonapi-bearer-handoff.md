<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# `tritonapi-skeleton` Bearer-auth handoff

Working-session handoff for whoever (fresh-context Claude or a human)
picks this up next. Pair with:

- `docs/design/branch-extraction-tritonapi-skeleton.md` — the PR
  extraction plan; Tier 4 covers the CLI/client work that this
  handoff expands on.
- `docs/tutorials/adding-a-tritonapi-feature.md` — pipeline and
  per-feature decision checklist. Still has a stubbed "worked
  example" section for `/v1/auth/login-ssh` that wants filling in
  now that the feature is live.

**Delete this file** once its remaining-work items have landed; it's
an operational artifact, not a long-lived design.

## What's live on coal today

A user with a standard SSH profile (keyId, url pointing at the
gateway, e.g. `https://localhost:8443` via the coal tunnel) can run a
complete session:

```
triton -p coal-gateway login               # SSH → JWT, stashed to ~/.triton/tokens/coal-gateway.json
triton -p coal-gateway login -u admin      # alt: password → JWT (LDAP/UFDS path)
triton -p coal-gateway whoami              # Bearer → GET /v1/auth/session
triton -p coal-gateway datacenters         # Bearer → gateway resigns with operator key → cloudapi
triton -p coal-gateway insts               # same, via Bearer
triton -p coal-gateway logout              # best-effort POST /v1/auth/logout + delete token file
```

After logout (or with no login), the CLI falls back to SSH-HTTP-Sig
for `/{account}/*` calls and refuses `whoami`/`logout` with a clear
"not logged in" message (those endpoints only accept Bearer).

Unmodified cloudapi clients (node-triton, terraform) also work
against the gateway unchanged — the gateway's `auth_scheme`
classifier routes HTTP-Sig through verbatim.

## Architecture as it stands

### Gateway (`services/triton-gateway/src/main.rs`)
- `auth_scheme(&HeaderMap)` classifier decides per-request:
  `Bearer` → JWKS verify + operator-key resign;
  `HttpSignature` → verbatim passthrough;
  `None` → verbatim passthrough (cloudapi 401s).
- Phase 0 error translation is gone — error bodies pass through
  verbatim. The client sees cloudapi's `{code,message}` shape on
  `/{account}/*` errors (Progenitor surfaces them as
  `InvalidResponsePayload`; acceptable).
- `gateway_error_response` emits cloudapi's legacy `{code, message,
  request_id}` shape for gateway-originated errors so node-triton
  can parse them.

### triton-api-server (`services/triton-api-server/src/main.rs`)
- `POST /v1/auth/login-ssh`: HttpSig-only; parses keyId
  `/{account}/keys/{fp}`, fetches the OpenSSH-or-PEM key from mahi
  (`account.keys[fp]`), verifies the signature, issues a JWT via the
  shared `issue_login_response` tail.
- `POST /v1/auth/login`: the pre-existing LDAP/UFDS password path,
  unchanged; now shares `issue_login_response` with login-ssh.
- `auth_scheme` classifier and `http_sig` verifier live as local
  modules in the server crate. **They should move to
  `libs/triton-auth`** — see Remaining work.
- Sig-verification failures (unknown account, wrong key on
  fingerprint, crypto reject, unparseable key blob) all collapse
  into one opaque `SignatureVerificationFailed` 401 to avoid
  account enumeration. ClockSkew / MalformedKeyId /
  SubuserKeyIdNotSupported / Malformed parser errors surface
  distinctly because they're client misconfigurations, not auth
  attempts.

### triton-gateway-client (`clients/internal/triton-gateway-client/`)
- `GatewayAuthMethod::Bearer { provider, account }` — the account
  field was added because `TypedClient::effective_account()` has to
  return a usable value on both branches; without it the CLI
  silently produced `/datacenters`-style URLs with no account
  prefix.
- `TokenProvider` trait (async, with `current_token` and
  `on_unauthorized` methods). The crate doesn't invoke
  `on_unauthorized` today — 401-retry lives at the CLI layer.

### triton-cli (`cli/triton-cli/`)
- `Cli::build_client()`: tries cached JWT via
  `commands::login::load_if_fresh` → Bearer; falls back to
  SSH-HTTP-Sig. Used by everything except `login`.
- `Cli::build_ssh_client()`: always SSH. Used by `Commands::Login`
  specifically — `/v1/auth/login-ssh` rejects Bearer by design.
- `CachedTokenProvider` in `src/commands/login.rs` — hands out the
  stashed JWT, errors on `on_unauthorized` so users see a clear
  "run triton login" message rather than a silent retry loop.
- Token storage at `~/.triton/tokens/<profile>.json`, mode 0600,
  atomic temp-file+rename write. JSON shape: `{token,
  refresh_token, username, user_id, email?, is_admin, issued_at}`.
  Outside the profile file intentionally so older CLIs don't trip
  on unfamiliar fields and a future Keychain/libsecret backend can
  slot in without churning the profile format.

## Recent commit chain

`git log origin/main..HEAD --oneline` gives the full picture. The
block since "Revise tritonapi extraction plan" is the consolidation
+ Bearer-login work:

1. Extraction plan revised, Phase 0 planned for removal.
2. Phase 0 gateway error translation reverted.
3. triton-cli reset to the pre-Phase-3 baseline (cloudapi-client
   only, no AnyClient).
4. Gateway gains HTTP-Signature passthrough via `auth_scheme`
   branching.
5. CLI consolidated on triton-gateway-client; cloudapi-client dep
   dropped from `cli/triton-cli`.
6. Feature-guide doc skeleton added at
   `docs/tutorials/adding-a-tritonapi-feature.md`.
7. `POST /v1/auth/login-ssh` declared (stub).
8. Auth classifier + HTTP-Sig verifier modules in
   `triton-api-server`.
9. `auth_login_ssh` handler: mahi lookup, verify, JWT issuance.
10. `triton login` CLI command, token storage.
11. PEM fallback in `parse_openssh_key`; CLI password-login via
    `-u/--user`.
12. Cached-JWT-as-Bearer via `Cli::build_client`; fix for
    `effective_account()` on Bearer branch.
13. `triton whoami` and `triton logout`.

## Remaining work, in priority order

### 1. Token refresh on 401 (next natural slice)

Today `CachedTokenProvider::on_unauthorized` errors out — a user
hitting an expired JWT sees "cached token was rejected; run triton
login". The stashed file already carries `refresh_token`; the
server endpoint `POST /v1/auth/refresh` exists (look at
`auth_refresh` in `services/triton-api-server/src/main.rs`). Real
refresh would:
- Detect expiry proactively (the `is_jwt_expired` helper in
  `login.rs` already shaves 30s off exp).
- On expiry, call auth_refresh with the stored refresh_token,
  rewrite the token file atomically, proceed.
- On auth_refresh failure, fall through to the "run triton login"
  error.

Touch points: `cli/triton-cli/src/commands/login.rs`
(CachedTokenProvider becomes mutable — likely an `Arc<Mutex<...>>`
holding the current access_token + refresh_token). Moderate scope,
maybe 200 LOC + tests.

### 2. Promote HTTP-Sig code into `libs/triton-auth`

Already-deferred refactor. Today three call sites do closely-related
things:
- Client signing: `libs/triton-auth/src/signature.rs` (outbound).
- Server verifying: `services/triton-api-server/src/http_sig.rs`
  (parser + verifier).
- Classifier: `services/triton-gateway/src/main.rs::auth_scheme`
  AND `services/triton-api-server/src/auth_scheme.rs` (two copies).

Target shape: `libs/triton-auth` grows a `http_sig` module with
parse + verify + classifier. Gateway and server both consume the
shared classifier. Verifier moves from server-local to lib. Same
wire behavior, one source of truth. Crypto deps (`rsa`, `p256`,
`p384`, `ed25519-dalek`) move with it.

Load-bearing for the "when the gateway goes away" story — all the
auth primitives survive the gateway's demise if they live in the
lib, not the gateway.

### 3. Sub-user keyId support

`POST /v1/auth/login-ssh` currently returns
`SubuserKeyIdNotSupported` 400 for keyIds like
`/{account}/users/{user}/keys/{fp}`. Enabling it needs an extra
mahi lookup (account → user → keys) and then JWT claims that carry
the sub-user identity rather than the account. Adjacent change:
mahi's sub-user shape is already in `apis/mahi-api/src/types/` (see
`AuthInfo::user`).

### 4. Worked example section in the feature-guide

`docs/tutorials/adding-a-tritonapi-feature.md` has a stub "Worked
example: POST /v1/auth/login-ssh" that was left to fill in once the
feature shipped. Now that it has, populate with actual file paths +
function names. Don't include commit SHAs (the repo squash-merges).

### 5. CLI §2 ValueEnum violation

Separate from this slice but flagged in the extraction plan. Can
wait for the libs/triton-auth refactor to finish so we know which
crate gets the fix.

### 6. Eventual PR extraction

Per Tier 4 of `docs/design/branch-extraction-tritonapi-skeleton.md`.
Big async task — decompose into review-sized PRs and land on main.

## Testing setup (as of handoff)

Port-forward:

```
ssh -L 8443:triton-api.coal.joyent.us:443 \
    -L 8444:cloudapi.coal.joyent.us:443 \
    coal-headnode
```

Profiles:
- `~/.triton/profiles.d/coal-gateway.json` — points at gateway
  (8443). Exercises the Bearer path after login, passthrough path
  before.
- `~/.triton/profiles.d/coal-direct.json` — points at cloudapi
  directly (8444). Control group; HTTP-Sig end-to-end without the
  gateway in the middle.

Admin credentials: `admin` / `joypass123` (UFDS/LDAP).

Redeploy cycle after any server-side change:
1. Push to `tritonapi-skeleton`. Jenkins builds both
   `images/triton-api` zone image and `images/tritonadm` tarball.
2. `ssh coal-headnode 'tritonadm self-update --latest --channel experimental'`.
3. `ssh coal-headnode 'tritonadm post-setup tritonapi -y --channel experimental'`.

Local CLI rebuild: `cargo build --release -p triton-cli` (takes
~25s warm).

## Things that are non-obvious

- **Mahi stores SSH public keys in PEM SubjectPublicKeyInfo
  format**, not OpenSSH `ssh-rsa AAAA...`. The verifier's
  `parse_openssh_key` tries OpenSSH first, falls back to PEM via
  `rsa::RsaPublicKey::from_public_key_pem` (and equivalents for
  ECDSA P-256, P-384, Ed25519), wrapping each through
  `ssh_key::public::<Algo>PublicKey` so the verifier API stays
  uniform. Features needed on the crypto crates in
  `services/triton-api-server/Cargo.toml`: `pkcs8`, `pem`.
- **The gateway's "operator signer" key on coal is registered as
  `triton-gateway` on the admin account**
  (fingerprint `0a:b6:46:8f:...`). When Bearer requests traverse
  the gateway, the gateway strips the Bearer and resigns as this
  operator key; cloudapi verifies against the admin account's keys
  in UFDS.
- **`tritonadm mahi get-account --login admin --raw`** was the
  decisive diagnostic for the PEM issue. Keep it in mind when
  login-ssh debugging goes opaque.
- **`effective_account()` on the Bearer branch** used to return
  `""` as a deferred TODO. That silently produced `/datacenters`
  (no account prefix) which cloudapi 404s as `"datacenters does not
  exist"`. Fixed by adding `account: String` to
  `GatewayAuthMethod::Bearer`; the CLI passes `profile.account` at
  `build_client` time.
- **`TRITON_PASSWORD` env var** is an undocumented escape hatch
  that `triton login -u <user>` honors for scripted/test flows,
  skipping the interactive prompt.
- **`Commands::Login` uses `build_ssh_client`** explicitly because
  `/v1/auth/login-ssh` rejects Bearer by design (the endpoint exists
  specifically to bootstrap a session from a fresh SSH signature).
  Every other command uses `build_client` which prefers Bearer when
  a cached token is fresh.

## Suggested next-session opener

> Read `docs/design/tritonapi-bearer-handoff.md` for branch state,
> then either (a) implement token refresh on 401 per "Remaining
> work" §1, or (b) promote the HTTP-Sig code into `libs/triton-auth`
> per §2. (a) is a smaller CLI-only slice; (b) is cross-crate but
> unblocks the "when the gateway goes away" narrative.
