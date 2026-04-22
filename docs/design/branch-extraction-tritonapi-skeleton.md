<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Decomposing `tritonapi-skeleton` into review-sized PRs

## Context

The `tritonapi-skeleton` branch has grown to ~135 commits / ~96k lines / 181
files. A single PR at this size is unreviewable. This doc identifies the
self-contained slices that can land on `main` ahead of the rest of the
branch, so reviewers see smaller, focused diffs and the genuinely tricky
parts (gateway, signer key, /v1 auth) can be reviewed in isolation on top of
an already-simplified remainder.

This is an operational planning doc, not a long-lived architecture doc.
Once the branch is fully decomposed and merged, this file can be removed.

## What's on the branch

- Five full **API conversions** (sapi, imgapi, papi, napi, mahi+mahi-sitter)
  — each follows the 5-phase restify-conversion pattern (plan → API trait →
  client → CLI/tritonadm integration → validation).
- A new admin CLI, **`tritonadm`**, that subsumed the short-lived
  `sapi-cli` / `imgapi-cli` crates.
- A new **zone image build** system (`images/triton-api/`,
  `image.defs.mk`, `deps/sdc-scripts` submodule).
- Two new services — **`triton-api-server`** (the /v1 API + auth) and
  **`triton-gateway`** (reverse proxy that splits /v1 to tritonapi vs.
  CloudAPI, signs CloudAPI requests, enforces JWTs).
- New shared libraries: **`triton-auth-session`** (ES256 JWT, UFDS LDAP,
  JWKS, mahi group resolution) and **`triton-tls`** (portable TLS loading).
- Ancillary cleanups (arch-lint, ring standardization, cloudapi-client error
  schema patch, restify-conversion skill tweaks).
- **CLI-side tritonapi integration** added in April: gateway error
  translation (`575c564`), merged triton-gateway OpenAPI spec
  (`c7347ff`), generated `triton-gateway-client` crate with pluggable
  Bearer/SSH auth (`f77b8dc`), CLI `Profile::{SshKey,TritonApi}` split +
  `FileTokenProvider` + `login`/`logout`/`whoami` (`fcc1d86`, `f2bb274`,
  `9a83f23`), and a 15-commit `AnyClient` dispatch rollout through the
  cloudapi command surface (`6c9575f..f48381b`, plus `a01077d` rbac).
  The direction of this last block is being revised — see Tier 4
  below.

## Dependency map

```
triton-tls   triton-auth (small additions)
   │              │
   │   ┌──────────┘
   │   │
   ▼   ▼
triton-auth-session ◄── mahi-client ◄── mahi-api
   │
   ├──► triton-api-server  (also needs triton-api)
   └──► triton-gateway

*-api  ──►  *-client  ──►  tritonadm  (needs all six clients)
                       └─► napi-cli, papi-cli (standalone)
```

Zone image infrastructure has **no Rust dependencies** — entirely
self-contained.

## Extractable PRs

Tiered so each PR builds cleanly on `main` once its prerequisites have
landed. "Size" is a rough reviewability indicator, not line count.

### Tier 0 — Standalone infrastructure (no ordering constraints)

| PR | Subject | Size |
|----|---------|------|
| PR-1 | Zone image build scaffolding (`images/`, `image.defs.mk`, `deps/sdc-scripts`, `docs/design/zone-image-builds.md`) | M |
| PR-2 | `triton-tls` crate extraction (`libs/triton-tls/`, ~160 LOC) | S |
| PR-3 | `triton-auth` enhancements (`legacy_pem.rs`, key_loader PEM normalization, `tests/fs_keys_test.rs`) | S |
| PR-4 | CloudAPI-client error schema patch + ring standardization + arch-lint + ImageError rename | S |
| PR-5 | restify-conversion skill improvements (`.claude/skills/restify-conversion/*`) | S |
| PR-6 | openapi-manager / client-generator transforms (the shared spec-patching infra). If decoupling from PR-7 is hard, bundle with PR-7. | M |

### Tier 1 — API conversions (each independent, can land in parallel after Tier 0)

Each PR delivers `apis/<svc>-api/`, generated + patched specs,
`clients/internal/<svc>-client/`, conversion plan, validation report. CLI
integration is deferred to Tier 2 when the CLI lives in `tritonadm`;
standalone CLIs (`papi-cli`, `napi-cli`) can ride along with their API+client.

| PR | Subject | Size |
|----|---------|------|
| PR-7 | SAPI API + client | L |
| PR-8 | IMGAPI API + client | L |
| PR-9 | PAPI API + client + standalone `cli/papi-cli/` | M |
| PR-10 | NAPI API + client + standalone `cli/napi-cli/` | L |
| PR-11 | mahi + mahi-sitter API + clients (prereq for `triton-auth-session`) | L |

### Tier 2 — `tritonadm` umbrella CLI (after Tier 1 clients land)

| PR | Subject | Size |
|----|---------|------|
| PR-12 | `tritonadm` scaffold + `docs/design/tritonadm.md` | M |
| PR-13 | `tritonadm image` + `tritonadm sapi` (subsumes the retired `imgapi-cli` and `sapi-cli`) | L |
| PR-14 | `tritonadm mahi` (Phase 4 of mahi conversion) | M |
| PR-15 | `tritonadm post-setup` (grafana, portal, common-external-nics, cloudapi, tritonapi) | L |
| PR-16 | Remaining subcommands (`dev`, `dc-maint`, `channel`, `platform`, `experimental`) | M |

### Tier 3 — tritonapi services (server side)

| PR | Subject | Size | Prereqs |
|----|---------|------|---------|
| PR-17 | `triton-api` API trait + skeleton server + openapi-manager registration | M | PR-2 |
| PR-18 | `triton-auth-session` library | L | PR-11 |
| PR-19 | `/v1/auth/*` endpoints + mahi group resolution wired into the server | M | PR-17, PR-18 |
| PR-20 | `triton-gateway` skeleton + CloudAPI proxy (HTTPS, HAProxy front, WS, graceful shutdown, request-ID, `/v1/*` routing) | L | PR-2 |
| PR-21 | Gateway JWT enforcement + CloudAPI request signing + signer key lifecycle | L | PR-18, PR-20 |

**Phase 0 gateway error translation (`575c564`) is dropped, not
landed.** It rewrote cloudapi's legacy error shape (`{code, message?,
request_id?}`) to the Dropshot-described shape (`{error_code, message,
request_id}`). That actively breaks unmodified node-triton / terraform
/ other cloudapi clients pointing at the gateway, because they parse
`error.code`. Since "unmodified cloudapi clients work against the
gateway" is the design target, the gateway must proxy error bodies
through verbatim. The commit does not appear in any extracted PR.

Consequence for Rust clients: error bodies from `/{account}/*` paths
arrive as `{code, message?}`, the Dropshot-generated `Error` schema
says `{error_code, message, request_id}` with required fields — so
Progenitor surfaces error responses as
`Error::InvalidResponsePayload(bytes, ...)`. This is the *same*
behavior `cloudapi-client` has today when pointed at the live Node
cloudapi directly — the CLI's error path already formats these
reasonably by falling back to the raw body. No regression; the
gateway-routed path simply stops papering over a pre-existing rough
edge.

The Dropshot-generated `cloudapi-api.json` and `triton-api.json` emit
the same `Error` shape at the spec level (verified by structural
diff: 92 shared component schemas bit-identical across cloudapi-api
and the `/{account}/*` subset of the merged gateway spec, including
`Error`). No schema unification work was needed, and none is needed.

### Tier 4 — tritonapi client + CLI consolidation

The client/CLI side of the April work landed in four phases. Phases 1–3
extract cleanly. **Phase 4 is being redone** under a simpler design;
the existing commits on the branch can land as-is to keep history
linear, but the eventual review-friendly version squashes/replaces
them with a much smaller change.

| PR | Subject | Size | Prereqs |
|----|---------|------|---------|
| PR-22 | Merged gateway OpenAPI spec + `openapi-manager/src/transforms.rs` (`c7347ff`) | M | PR-6, PR-17 |
| PR-23 | `triton-gateway-client` crate (Progenitor-generated from the merged spec, Bearer/SSH pluggable auth) (`f77b8dc`) | L | PR-22 |
| PR-24 | `libs/triton-pagination` extraction (`d9d01f4`) + `triton-tls` crypto provider install (`9f2ca1a`) | S | PR-2 |
| PR-25 | CLI Bearer profiles: `Profile::{SshKey,TritonApi}` split, `FileTokenProvider`, `login`/`logout`/`whoami` (`fcc1d86`, `f2bb274`, `9a83f23`) | L | PR-23 |
| PR-26 | Ship `triton` CLI inside the tritonadm tarball (`447d211`) | S | PR-12, PR-25 |
| PR-27 | **CLI consolidation on `triton-gateway-client`** — drop `cloudapi-client` dependency from `cli/triton-cli`; handlers talk one client with `GatewayAuthMethod::{SshKey,Bearer}` chosen per profile. Replaces `AnyClient` / `dispatch!` / `dispatch_with_types!` / the per-command dispatch port commits (`6c9575f..f48381b`, `a01077d`). | L | PR-25 |

### Change of direction on CLI client dispatch

Phase 4 (as currently landed) wraps every cloudapi HTTP call in a
runtime-enum dispatch between `cloudapi_client::TypedClient` (SSH) and
`triton_gateway_client::TypedClient` (Bearer JWT), with
`dispatch!` / `dispatch_with_types!` macros that textually duplicate
each handler body across two match arms. This was built on the premise
that the two Progenitor-generated clients have structurally identical
but nominally distinct builder/request/response types that no trait
signature can unify.

**That premise is correct but the duplication is avoidable.** A
structural diff of the two specs shows:

- 62/62 `/{account}/*` paths match exactly.
- 92/92 shared component schemas are bit-identical. `triton-gateway`'s
  spec is a pure set-union: cloudapi-api schemas unchanged + 10 new
  tritonapi-native schemas (Jwk, JwkSet, Login*, Refresh*, Session*,
  Ping*, UserInfo).
- The only per-path differences are two cosmetic Dropshot artifacts
  (null-body schemas on `update_machine` and
  `start_machine_from_snapshot` 202 responses) that Progenitor compiles
  to identical Rust either way.

So `triton-gateway-client`'s Progenitor output for the `/{account}/*`
surface is wire-equivalent to `cloudapi-client`'s. One client is
enough.

The consolidated design (PR-27):

- `cli/triton-cli` drops the `cloudapi-client` dependency entirely and
  depends only on `triton-gateway-client`.
- Auth selection is a one-line choice made in
  `Cli::build_client()`: SSH profiles get
  `GatewayAuthMethod::SshKey(AuthConfig)`, tritonapi profiles get
  `GatewayAuthMethod::Bearer(Arc<dyn TokenProvider>)`. Both variants
  already exist on `GatewayAuthConfig` (`libs/triton-gateway-client/src/auth.rs`).
- SSH profiles point the same client at the cloudapi URL directly,
  which works because the `/{account}/*` Progenitor output is
  structurally cloudapi. Tritonapi profiles point at the gateway URL
  and use Bearer auth.
- `AnyClient`, `dispatch!`, `dispatch_with_types!`, and all
  `serde_json::from_value(serde_json::to_value(x)?)?` round-trips at
  dispatch boundaries delete.
- The CLAUDE.md §2 violation (`clap::ValueEnum` not derivable on
  canonical API types when multiple Progenitor copies exist)
  evaporates, since only one set of Progenitor types exists in
  `cli/triton-cli`.

**Things that must be preserved in the consolidation**:

- The 410-Gone `get_machine` recovery in
  `clients/internal/cloudapi-client/src/lib.rs:1060–1118` — the one
  hand-written wrapper around a Progenitor method in cloudapi-client.
  Either port it to a `TypedClient` wrapper on `triton-gateway-client`
  or keep the behavior inline in the instance handler.
- `EMIT_PAYLOAD_SENTINEL` + `set_emit_payload_mode` debug harness
  (cloudapi-client only today). Port to `triton-gateway-client`.
- The `WebsocketAuth::{HttpSignature,Bearer}` shape for out-of-band WS
  upgrades (`cli/triton-cli/src/client.rs:107-138`). Stays
  substantially the same; source collapses because only one client
  provides `auth_config()`.

**Phase 0 is dropped alongside this cleanup** (see Tier 3 note).
Without gateway error translation, Rust Progenitor clients see
`Error::InvalidResponsePayload` on cloudapi error bodies —
identical to today's `cloudapi-client` behavior when pointed at the
live Node cloudapi, so no new regression. The CLI's existing error
formatting already handles this path. The win is that node-triton,
terraform-provider-triton, and other unmodified cloudapi clients can
point at the gateway and Just Work, which is the whole point of the
"thin shim" direction.

**Sequencing note**: PR-27 is a destructive replacement of the
15-commit dispatch rollout. Two ways to land it:

1. **Rewrite history on the extraction branch**: cherry-pick
   PR-22…PR-26 onto main, then author PR-27 as a single diff that
   never introduces `AnyClient`. The `6c9575f..f48381b` / `a01077d`
   commits never appear upstream.
2. **Land as-is then cleanup**: ship the current Phase 4 dispatch
   commits, then land PR-27 as a follow-up that deletes the dispatch
   infrastructure. Easier to review in isolation; adds and immediately
   removes ~1500 lines of macro/dispatch code to mainline history.

Recommend (1) — the dispatch infrastructure had a two-week useful life
on an experimental branch and does not belong in the public history of
`main`.

### Work deferred from Tier 4

Called out here so nothing gets lost:

- **`image copy` cross-DC under JWT auth** — per-DC JWT issuance
  means copying from DC-A to DC-B requires a DC-B token. Recommend a
  `--destination-profile <name>` flag pointing at a pre-logged-in
  tritonapi profile. Land after PR-27.
- **`ImageCache` type migration** — currently persists
  `cloudapi_client::types::Image`. Once PR-27 removes
  `cloudapi-client`, the cache must store either
  `triton_gateway_client::types::Image` or the canonical
  `cloudapi_api::types::Image`. Prefer the latter (stable across
  future spec regenerations).
- **Structured error handling in Rust clients** — with Phase 0 gone,
  Rust clients see `Error::InvalidResponsePayload` on cloudapi error
  bodies because the live Node cloudapi emits `{code, message?,
  request_id?}` but the Dropshot-generated spec says `{error_code,
  message, request_id}`. Two paths to recover structured errors:
  (a) modernize the Node cloudapi service's error emission to match
  the Dropshot schema, or (b) patch the cloudapi-api OpenAPI spec
  to describe the real legacy shape and regenerate clients. (a) is
  the right long-term move but is cross-repo; (b) is local but
  propagates legacy naming into Rust. File as a follow-up bead.

## Suggested landing order

By reviewability, not strict dependency:

1. The Tier-0 paperwork PRs (PR-1 → PR-6). Clears a lot of noise cheaply.
2. Tier-1 API+client PRs in parallel — independent reviewers can each take one.
3. tritonadm scaffold (PR-12), then subcommand PRs incrementally.
4. tritonapi server side: PR-17 → PR-18 → PR-19 → PR-20 → PR-21.
5. tritonapi client/CLI: PR-22 → PR-23 → PR-24 → PR-25 → PR-26 → PR-27.

Residual on `tritonapi-skeleton` after all of these is nothing; the
branch can be closed or rebased and merged as a thin capstone.

## Verification

Before opening any PR, sanity-check the slice:

```sh
# Confirm commits cleanly partition by path
git log --oneline main..HEAD -- <paths>

# Confirm reviewer scale
git diff --stat main...HEAD -- <paths>

# Cherry-pick the slice onto main and verify it builds in isolation
git checkout -b verify-pr-N main
git cherry-pick <commits>
make package-build PACKAGE=<crate>
make package-test PACKAGE=<crate>
```

For any API-conversion PR, also run `make openapi-check` to confirm the
spec files match the traits.
