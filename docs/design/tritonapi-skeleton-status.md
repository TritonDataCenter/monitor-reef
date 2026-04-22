# `tritonapi-skeleton` branch status

**Last updated:** 2026-04-22. Delete this file when the branch is decomposed and merged.

This is a working-session handoff so a fresh Claude (or a human reviewer) can pick up without replaying the conversation that produced the branch. Pair with `docs/design/branch-extraction-tritonapi-skeleton.md` (PR-extraction memo) and `docs/design/tritonadm-distribution.md` (image/shar distribution).

## Commit range (oldest → newest)

```
575c564 Translate CloudAPI error bodies to tritonapi shape in triton-gateway
c7347ff Emit merged triton-gateway OpenAPI spec
f77b8dc Add triton-gateway-client with pluggable Bearer/SSH auth
9f2ca1a triton-tls: own process-wide rustls crypto provider install
447d211 Ship the triton CLI inside the tritonadm tarball
fcc1d86 triton-cli: split Profile into SshKey / TritonApi enum variants
f2bb274 triton-cli: add FileTokenProvider + on-disk token storage
9a83f23 triton-cli: add login / logout / whoami commands
6c9575f triton-cli: Phase 4 first slice — AnyClient dispatch, three ports
d9d01f4 Extract paginate_all into triton-pagination crate
e9df977 triton-cli: port instance-family + package commands to AnyClient
0ba0c84 triton-cli: port key / accesskey / account commands to AnyClient
2fcd587 triton-cli: clippy fixes after Phase 4 port
dc39a75 triton-cli: port image commands to AnyClient
625c272 triton-cli: port network commands to AnyClient
0f9941b triton-cli: port fwrule commands to AnyClient
1acb4a1 triton-cli: port vlan commands to AnyClient
184aa90 triton-cli: port volume commands to AnyClient
39eea08 triton-cli: port instance create to AnyClient
0df1794 triton-cli: port changefeed to AnyClient (JWT-over-WebSocket)
f48381b triton-cli: port instance vnc to AnyClient via WebsocketAuth
```

Everything through `f48381b` is on branch; previous push landed through `9a83f23`, so `6c9575f..f48381b` is the unpushed block at the time of this writing — check `git log origin/tritonapi-skeleton..HEAD` for the current gap.

## What's landed, by phase

### Phase 0 — gateway error translation (`575c564`)

`services/triton-gateway/src/error_translate.rs` + call sites in `proxy_to_cloudapi`. Every non-2xx response leaving the gateway now carries a uniform tritonapi-shaped `Error` (`{error_code?, message, request_id}`), regardless of whether the upstream is cloudapi (`{code, message?, request_id?}`) or the gateway itself (auth failures, etc.). Preserves upstream status + non-content headers. WebSocket upgrades explicitly short-circuit translation — touching the body would break the 101 handshake. 15 unit/integration tests.

### Phase 1 — merged gateway OpenAPI spec (`c7347ff`)

`openapi-specs/patched/triton-gateway-api.json` — 13,159 lines, 68 paths (62 cloudapi `/{account}/*` + 6 tritonapi `/v1/*`). Merge logic in `openapi-manager/src/transforms.rs`, ported from mariana-trench with the path-rewrite + `x-datacenter` header injection dropped. The `Error` schema collision between cloudapi and tritonapi is resolved in favor of tritonapi's shape — correct because Phase 0 rewrites cloudapi errors at the gateway. `make openapi-check` validates freshness.

### Phase 2 — `triton-gateway-client` crate (`f77b8dc`)

`clients/internal/triton-gateway-client/`. Progenitor-generated from the merged spec (37,537 line `generated.rs`, checked in). Pluggable auth via `GatewayAuthMethod::{Bearer(Arc<dyn TokenProvider>), SshKey(AuthConfig)}`. `TokenProvider` trait is defined here but the gateway-client itself doesn't invoke `on_unauthorized` — the Phase 3 CLI `FileTokenProvider` handles reactive refresh.

### Phase 3 — tritonapi profiles + login/logout/whoami (`fcc1d86`, `f2bb274`, `9a83f23`)

- Profile enum at `cli/triton-cli/src/config/profile.rs`: `#[serde(tag = "auth")]` discriminator over `SshKey(SshKeyProfile)` / `TritonApi(TritonApiProfile)`. Custom deserializer keeps old SSH-profile JSON loading unchanged (no `auth` field → SSH variant).
- Token storage at `~/.triton/tokens/<profile>.json` (mode 0600, atomic `.new`+rename writes). Module at `cli/triton-cli/src/auth/{mod,tokens,jwt,token_provider}.rs`.
- `FileTokenProvider::load(profile, gateway_url, insecure) -> Arc<Self>` implements the gateway-client's `TokenProvider` trait with proactive (expiry - 30s) refresh and reactive `on_unauthorized()`.
- New top-level commands `login`, `logout`, `whoami` with axum-backed integration test.
- JWT `exp` decoded with a 20-line base64 helper in `auth/jwt.rs`, no `jsonwebtoken` dep.
- UX decisions documented in the commit messages; notably `TRITON_PASSWORD` env var exists as an undocumented escape hatch for non-tty test flows.

### Phase 4 — route cloudapi commands through gateway for tritonapi profiles (`6c9575f` onward)

Approach: runtime-enum dispatch, not trait abstraction. Progenitor generates per-crate builder types that aren't interchangeable, so no trait signature unifies `cloudapi_client::builder::ListMachines<'_>` with `triton_gateway_client::builder::ListMachines<'_>` — a match arm is the only honest answer.

Key types in `cli/triton-cli/src/client.rs`:
- `enum AnyClient { CloudApi { client, insecure }, Gateway { client, account, insecure } }`
- `macro_rules! dispatch!($client, |$c:ident| $body:block)` — textually substitutes `$body` into each match arm.
- `macro_rules! dispatch_with_types!($client, |$c, $t| $body:block)` — same, plus per-arm `use <crate>::types as $t;` for handlers that build strongly-typed request bodies.
- `enum WebsocketAuth { HttpSignature(AuthConfig), Bearer(Arc<dyn TokenProvider>) }` — clonable auth source for out-of-band WS upgrades. `AnyClient::websocket_auth()` extracts one.
- `pub async fn WebsocketAuth::headers(&self, path) -> Result<(Option<String>, String)>` — returns the `(Date, Authorization)` pair to stamp on an upgrade request.

`Cli::build_any_client()` in `main.rs` dispatches on profile kind. SSH profiles delegate to the existing `build_client()` and wrap; tritonapi profiles load tokens via `FileTokenProvider`, construct a `GatewayAuthConfig::bearer(...)`, build a gateway TypedClient.

Ported handlers (~93 total):
- `datacenters`, `services`, `info`
- `instance list/get/ip/ssh/audit/rename/resize/firewall/protection/delete/create/start/stop/reboot`
- `instance disk/metadata/migration/nic/snapshot/tag` subcommands
- `instance wait` (state helper used by several lifecycle commands)
- `instance changefeed`, `instance vnc` (WebSocket — Bearer-auth'd upgrades verified against real gateway via changefeed)
- `image list/get/create/delete/clone/update/export/share/unshare/wait/tag`
- `network list/get/default get/set/create/delete/ip list/get/update`
- `fwrule list/get/create/delete/enable/disable/update/instances`
- `vlan list/get/create/delete/update/networks`
- `volume list/get/create/delete/sizes`
- `package list/get`
- `key list/get/create/delete`, `accesskey list/get/create/update/delete`
- `account get/update`

Not ported (intentional):
- `rbac *` — see "Remaining work" below
- `image copy` cross-DC write — see "Remaining work" below
- `cloudapi` raw-request debug command — cloudapi-client-only by design

### Misc infrastructure

- `libs/triton-pagination/` (`d9d01f4`) — `paginate_all` moved out of cloudapi-client into a tiny shared crate; both clients re-export as `pagination`.
- `libs/triton-tls`: `install_default_crypto_provider` (`9f2ca1a`) and `build_rustls_client_config(insecure)` (`0df1794`) for callers that need raw TLS (WebSocket upgrades).
- `apis/cloudapi-api` gained `Clone` on `Machine`, `MachineDisk`, `MachineNic` (needed by ported handlers).
- `tools/install-tritonadm.sh` + `images/tritonadm/Makefile` bundle the `triton` binary alongside `tritonadm` (`447d211`) — one install on a headnode gets both.
- `Jenkinsfile` at repo root builds both `images/triton-api` and `images/tritonadm` stages.

## Remaining work

### `rbac *` port (mechanical, big)

5,373 LOC across 10 files under `cli/triton-cli/src/commands/rbac/`. `apply.rs` alone is 1,749 lines of declarative-config orchestration (reads a YAML/JSON file and syncs users/roles/policies to match). No new toolkit needed — mechanical `dispatch!` / `dispatch_with_types!` application following the Phase 4 pattern. Architecturally fine: cloudapi RBAC endpoints are in the merged gateway spec and the gateway proxies them; a JWT-auth'd operator managing their own sub-users is legitimate. Good sub-agent task.

### `image copy` cross-DC write

Current handler at `cli/triton-cli/src/commands/image.rs:894` bails on the `Gateway` variant because the destination-DC client reuses the source's `AuthConfig`, and gateway profiles don't own a cloudapi AuthConfig. Real underlying issue: **JWTs are per-DC** (each gateway has its own signing key, its own refresh-token store). To copy from DC-A to DC-B under JWT auth, the CLI needs a DC-B token too.

Design question to resolve before porting:
- Option A: `--destination-profile <name>` flag pointing at a pre-logged-in tritonapi profile for the dest DC.
- Option B: Auto-detect the dest gateway URL from the `list_datacenters` map, prompt interactively for DC-B credentials on-the-fly.
- Option C: Document the limitation, keep SSH-only, add a bead for later.

I'd pick A — explicit, composes with existing `triton login` machinery, no interactive-prompt surprise.

### CLAUDE.md Type Safety Rules §2 violation

Several `cloudapi_api::*` enums used as CLI args (`MachineState`, `Brand`, `MachineType`, `ImageState`, `ImageType`, `NicState`, `VolumeState`, `VolumeType`) lack `clap::ValueEnum`. Phase 4 handlers work around this with one-line serde round-trips (`serde_json::from_value(serde_json::to_value(x)?)?`) at the CLI-arg boundary. Fix is either: add the derive directly on the canonical API types, or introduce a feature flag on `cloudapi-api` that pulls `clap` as an optional dep. Would eliminate a class of conversion noise across the ported handlers.

### `instance get` 410 Gone regression

`cloudapi-client::TypedClient::get_machine` had special-case handling to recover a `Machine` from `InvalidResponsePayload` on a 410 Gone (for deleted-but-still-listed machines). The raw-builder approach used in the port lost this. Blast radius small — only affects `triton instance get <uuid>` on a VM cloudapi flagged as gone. Either duplicate the recovery inline in the handler or build a shared helper.

### `ImageCache` stores Progenitor types

`cli/triton-cli/src/cache.rs` persists `cloudapi_client::types::Image` (Progenitor's per-crate newtype). Phase 4 handlers round-trip at the cache boundary to get the canonical `cloudapi_api::types::Image`. Cleaner fix: migrate the cache to store canonical types directly; touches `cache.rs`, `commands/image.rs`, `commands/instance/list.rs`.

### `with_replacement` (deferred)

We explored Progenitor's `with_replacement` to make cloudapi-client and triton-gateway-client reference the same `cloudapi_api::types::Machine` etc., which would let dispatch arms return these types directly (instead of extracting fields inside the block). It works mechanically, but `Machine` transitively references ~10 types (`Brand`, `MachineState`, `MachineType`, `Metadata`, `Tags`, `Timestamp`, `MachineNic`, `MachineDisk`, `RoleTags`). Replacing just `Machine` without its dependents produces field-type mismatches. The full replacement is a multi-crate refactor with real surface-area risk, for a payoff of ~100 lines of cleanup in Phase 4 handlers. Not worth it now; revisit if the inside-block pattern becomes painful at scale.

## Live-test setup (as of last verification)

Against coal via an SSH tunnel from the user's workstation:

```
ssh -L 8443:triton-api.coal.joyent.us:443 coal-headnode
```

Local profile at `~/.triton/profiles.d/coal-local.json`:
```json
{
  "auth": "tritonapi",
  "url": "https://localhost:8443",
  "account": "admin",
  "insecure": true
}
```

Admin credentials for login: `admin` / `joypass123` (LDAP/UFDS on coal).

End-to-end verified: `triton -p coal-local login/whoami/logout`, `datacenters`, `services`, `info`, `insts`, `pkgs`, `keys`, `account get`, `imgs`, `nets`, `fwrules`, `image get`, `network get`, `changefeed`, `instance vnc --url-only`. Some commands hit genuine server-side 4xx/5xx on coal (no bhyve VMs → 400 VNC upgrade; no volumes service → 405 `vols`; fabrics disabled → 501 `vlan list`) — those are server config, not CLI bugs, and they all come back properly wrapped in the tritonapi `Error` shape thanks to Phase 0.

Binary rebuild after any CLI change:
```
PATH=rust/cargo/bin:$PATH CARGO_HOME=rust/cargo rust/cargo/bin/cargo build --release -p triton-cli
```

For testing against the coal headnode directly (not via tunnel), `sdcadm experimental get-tritonadm --latest` on the headnode pulls the Jenkins-built tarball containing both binaries.

## Architecture conventions worth preserving

- **`cloudapi-client` is not modified in Phase 4.** Auth pluggability lives inside `triton-gateway-client`'s `GatewayAuthConfig`. If a future slice needs to add bearer support to cloudapi-client directly, that's a deliberate design change, not an opportunistic refactor.
- **Dispatch macros stay in `cli/triton-cli/src/client.rs`.** They're `#[macro_export]` so accessible as `crate::dispatch!` / `crate::dispatch_with_types!`.
- **Return std types from inside the dispatch block** when Progenitor-generated per-crate newtypes would escape the match arms (the `info.rs` pattern). Alternative: render/serialize inside the dispatch and return `()` or `serde_json::Value`.
- **`effective_account()` on `AnyClient`** returns the profile's configured account. For gateway profiles, callers may alternatively use the literal `"my"` in path parameters — the gateway rewrites — but explicit is clearer.
- **`baseurl()` / `insecure()` accessors** on `AnyClient` for out-of-band consumers that bypass Progenitor (WebSockets, debug-output paths).
- **Error shapes stay per-crate** (gateway `Error` has `error_code`, cloudapi `Error` has `code`). The Phase 0 runtime translation keeps the wire surface consistent; handler code should not inspect Progenitor `Error` payloads directly anyway.

## Known audit advisories (not regressions)

CLAUDE.md lists four; the actual set on this tree is broader. All pre-exist Phase 0. Documented for reference, do not block on:

```
RUSTSEC-2023-0071, RUSTSEC-2024-0436, RUSTSEC-2025-0009, RUSTSEC-2025-0010,
RUSTSEC-2025-0134, RUSTSEC-2026-0009, RUSTSEC-2026-0049, RUSTSEC-2026-0097,
RUSTSEC-2026-0098, RUSTSEC-2026-0099
```

A follow-up to refresh CLAUDE.md's exception list is worth doing.

## Suggested next-session opener

> Read `docs/design/tritonapi-skeleton-status.md` for branch state. Then port the `rbac *` family to `AnyClient` following the Phase 4 toolkit. Brief a sub-agent.
