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

### Tier 3 — tritonapi services

| PR | Subject | Size | Prereqs |
|----|---------|------|---------|
| PR-17 | `triton-api` API trait + skeleton server + openapi-manager registration | M | PR-2 |
| PR-18 | `triton-auth-session` library | L | PR-11 |
| PR-19 | `/v1/auth/*` endpoints + mahi group resolution wired into the server | M | PR-17, PR-18 |
| PR-20 | `triton-gateway` skeleton + CloudAPI proxy (HTTPS, HAProxy front, WS, graceful shutdown, request-ID, `/v1/*` routing) | L | PR-2 |
| PR-21 | Gateway JWT enforcement + CloudAPI request signing + signer key lifecycle | L | PR-18, PR-20 |

## Suggested landing order

By reviewability, not strict dependency:

1. The Tier-0 paperwork PRs (PR-1 → PR-6). Clears a lot of noise cheaply.
2. Tier-1 API+client PRs in parallel — independent reviewers can each take one.
3. tritonadm scaffold (PR-12), then subcommand PRs incrementally.
4. tritonapi services: PR-17 → PR-18 → PR-19 → PR-20 → PR-21.

Residual on `tritonapi-skeleton` after all of these is nearly nothing; the
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
