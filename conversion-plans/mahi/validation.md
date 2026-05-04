<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Mahi Conversion Validation Report

## Summary

| Category             | Status | Notes                                                           |
|----------------------|--------|-----------------------------------------------------------------|
| Endpoint Coverage    | PASS   | 28/28 Restify routes exposed via `tritonadm mahi …`             |
| Wire-Format Patches  | PASS   | All four patch targets verified in `openapi-specs/patched/…`    |
| Type Completeness    | PASS   | All documented quirks modeled in `apis/mahi-api/src/types/`     |
| CLI Coverage         | PASS   | Every endpoint mapped to a `MahiCommand` / `MahiSitterCommand`  |
| Type-Safety Audit    | PASS   | No hand-defined enums; no `{:?}` in user-facing output          |
| Build + Tests        | PASS   | `make check` — 1272/1272 tests pass                             |
| OpenAPI Freshness    | PASS   | `make openapi-check` / `make clients-check` clean               |
| `make lint`          | PASS   | `cargo clippy --all-targets --all-features -- -D warnings` clean |

**Overall status: READY TO SHIP** for the API-trait / client / CLI surface.
No Rust mahi-server implementation exists yet; that remains a separate
orchestration-level decision (noted in plan.md, not a conversion gap).

## Endpoint Coverage

Source routes enumerated from `lib/server/server.js` and
`lib/replicator/server.js`. All 28 routes are reachable from tritonadm:

### `MahiApi` — public mahi server (`--mahi-url` / `MAHI_URL`)

| Source route                                        | tritonadm invocation                                           | Result   |
|-----------------------------------------------------|----------------------------------------------------------------|----------|
| `GET /accounts/:accountid`                          | `tritonadm mahi get-account-by-uuid <uuid>`                    | verified |
| `GET /accounts?login=`                              | `tritonadm mahi get-account --login <login>`                   | verified |
| `GET /users/:userid`                                | `tritonadm mahi get-user-by-uuid <uuid>`                       | verified |
| `GET /users?account=&login=&fallback=`              | `tritonadm mahi get-user --account --login [--fallback]`       | verified |
| `GET /roles?account=&role=`                         | `tritonadm mahi get-role-members --account [--role]`           | verified |
| `GET /uuids?account=&type=&name=…`                  | `tritonadm mahi name-to-uuid --account --type --name …`        | verified |
| `GET /names?uuid=…`                                 | `tritonadm mahi uuid-to-name --uuid …`                         | verified |
| `GET /ping`                                         | `tritonadm mahi ping`                                          | verified |
| `GET /lookup`                                       | `tritonadm mahi lookup`                                        | verified |
| `GET /account/:account` (deprecated)                | `tritonadm mahi get-account-old <login>`                       | verified |
| `GET /user/:account/:user` (deprecated)             | `tritonadm mahi get-user-old <account> <user>`                 | verified |
| `POST /getUuid` (deprecated)                        | `tritonadm mahi name-to-uuid-old …`                            | verified |
| `POST /getName` (deprecated)                        | `tritonadm mahi uuid-to-name-old --uuid …`                     | verified |
| `GET /aws-auth/:accesskeyid`                        | `tritonadm mahi get-user-by-access-key <id>`                   | verified |
| `POST /aws-verify`                                  | `tritonadm mahi verify-sig-v4 --method --url [--body]`         | verified |
| `POST /sts/assume-role`                             | `tritonadm mahi sts-assume-role --body <json>`                 | verified |
| `POST /sts/get-session-token`                       | `tritonadm mahi sts-get-session-token --body <json>`           | verified |
| `POST /sts/get-caller-identity` (XML)               | `tritonadm mahi sts-get-caller-identity --body <json>`         | verified |
| `POST /iam/create-role`                             | `tritonadm mahi iam-create-role …`                             | verified |
| `GET /iam/get-role/:roleName`                       | `tritonadm mahi iam-get-role <name> --account-uuid`            | verified |
| `POST /iam/put-role-policy`                         | `tritonadm mahi iam-put-role-policy …`                         | verified |
| `DEL /iam/delete-role/:roleName`                    | `tritonadm mahi iam-delete-role <name> --account-uuid`         | verified |
| `DEL /iam/delete-role-policy`                       | `tritonadm mahi iam-delete-role-policy …`                      | verified |
| `GET /iam/list-roles`                               | `tritonadm mahi iam-list-roles --account-uuid [--max-items]`   | verified |
| `GET /iam/list-role-policies/:roleName`             | `tritonadm mahi iam-list-role-policies <name> …`               | verified |
| `GET /iam/get-role-policy/:roleName/:policyName`    | `tritonadm mahi iam-get-role-policy <role> <policy> …`         | verified |

### `MahiSitterApi` — replicator admin (`--mahi-sitter-url` / `MAHI_SITTER_URL`)

| Source route         | tritonadm invocation                                    | Result   |
|----------------------|---------------------------------------------------------|----------|
| `GET /ping`          | `tritonadm mahi sitter ping`                            | verified |
| `GET /snapshot`      | `tritonadm mahi sitter snapshot [--output <path>]`      | verified |

No missing endpoints. 28/28.

## Wire-Format Checks

### `POST /sts/get-caller-identity` — XML body

- **Trait**: `apis/mahi-api/src/lib.rs:300` returns
  `Result<Response<Body>, HttpError>`.
- **Patched spec**: `openapi-specs/patched/mahi-api.json:2062` declares
  `{ "text/xml": { "schema": { "type": "string" } } }` with description
  *"XML body with Content-Type: text/xml"*.
- **CLI**: `cli/tritonadm/src/commands/mahi.rs:766-785` collects the
  `ByteStream` into a `Vec<u8>` via `TryStreamExt::try_collect`, then
  `String::from_utf8` and `println!("{xml}")`. No full buffering issue here
  — caller identity XML is small.

### `GET /snapshot` (sitter) — binary stream, 201

- **Trait**: `apis/mahi-api/src/lib.rs:437` returns
  `Result<Response<Body>, HttpError>`.
- **Patched spec**: `openapi-specs/patched/mahi-sitter-api.json:70-74` —
  status `"201"`, content type `application/octet-stream`,
  schema `{"type": "string", "format": "binary"}`.
- **CLI**: `cli/tritonadm/src/commands/mahi.rs:1031-1075` streams chunk-by-
  chunk to either stdout (via a locked stdout guard) or `--output <path>`
  (via `tokio::fs::File::write_all`). Never accumulates the full body in
  memory — matches the requirement.

### `GET /uuids?name=a&name=b` and `GET /names?uuid=x&uuid=y`

- **Patched spec** declares both as `{ style: "form", explode: true,
  schema: { type: "array", items: { type: "string" } } }`
  (`openapi-specs/patched/mahi-api.json:2277` for `name` on `/uuids`;
  `openapi-specs/patched/mahi-api.json:1899` for `uuid` on `/names`).
- **Progenitor signature**: Because of the spec patch, the generated
  builder accepts `Vec<String>` directly on `.name(...)` / `.uuid(...)`.
- **CLI**: `cli/tritonadm/src/commands/mahi.rs:116` and `:127` accept
  `Vec<String>` with repeatable `--name` / `--uuid` flags. The Vec is
  passed through unchanged to the Progenitor builder (no comma-join
  needed — the form/explode Progenitor machinery handles repeats).

### `ListRolesResponse` mixed casing

- **Type**: `apis/mahi-api/src/types/iam.rs:204-211` — no struct-level
  `rename_all`; field-level `#[serde(rename = "IsTruncated")]` and
  `#[serde(rename = "Marker", default, skip_serializing_if = ...)]`.
  `roles` stays lowercase by default. Matches upstream wire shape exactly.
- **Spec**: `openapi-specs/patched/mahi-api.json:679-718` documents
  `{ "roles": [...], "IsTruncated": bool, "Marker": string|null }`.

### `list-role-policies` — `maxitems` with `maxItems` alias

- **Type**: `apis/mahi-api/src/types/iam.rs:106-107` declares the field as
  `maxitems: Option<u32>` with
  `#[serde(default, rename = "maxitems", alias = "maxItems")]`. Serde
  accepts either spelling inbound.
- **CLI**: `cli/tritonadm/src/commands/mahi.rs:381` exposes the flag as
  `--maxitems` (lowercase), consistent with the upstream primary spelling.

## Documented Behavior Fixes

### `GET /roles` missing-role hang fix

Plan Phase 1 recommends returning 404 `RoleDoesNotExist` in the Rust
implementation (upstream hangs when the role is missing). The plan
captures this in the **"Phase 2b — service-layer behavior requirements"**
section (`conversion-plans/mahi/plan.md`, item 4). No Rust mahi server
implementation exists in this repo; the trait is ready to implement this
404 path whenever a Rust server is written. No beads issue filed per the
instructions.

### `GET /users?fallback=true` modeling

- **Type**: `apis/mahi-api/src/types/common.rs:229-232` — `AuthInfo` has
  `pub user: Option<User>` and `pub roles: HashMap<_, _>`. Serde's default
  for `Option` is `None`; `HashMap` default is empty. This correctly
  represents the fallback branch where the sub-user is missing.
- **Query struct**: `apis/mahi-api/src/types/lookup.rs:67-71` — `fallback`
  is `Option<bool>` with documentation noting the fallback branch returns
  `AuthInfo` with no user and an empty roles map.

Open wire-level verification follow-up filed as a beads issue (see below).

## Type-Safety Audit

- **No hand-defined enums in tritonadm.** `ObjectType`, `CredentialType`,
  and `ArnPartition` all derive `clap::ValueEnum` on the canonical API
  types (`apis/mahi-api/src/types/common.rs:21, 50, 67`). The
  client-generator `with_patch` entries at
  `client-generator/src/main.rs:162-164` apply the same derive to the
  Progenitor-generated copies, satisfying rules #2 and #3.
- **No `{:?}` in user-facing strings** aside from the defensive fallback
  inside `enum_to_display` (`cli/tritonadm/src/commands/mahi.rs:27`),
  which is the standard pattern in this repo.
- **No duplicate enum definitions** anywhere in `cli/tritonadm/src/commands/mahi.rs`.
- **Re-export pattern** — the CLI imports types exclusively via
  `mahi_client::types::*`, matching the repo convention.

## Build / Check Status

| Command                           | Result |
|-----------------------------------|--------|
| `make openapi-check`              | PASS — 11 docs fresh, 8 patched docs fresh |
| `make clients-check`              | PASS — all 10 clients up-to-date |
| `make lint`                       | PASS — `cargo clippy -D warnings` clean; ast-grep reports 1 pre-existing warning in `services/triton-api-server/src/main.rs:288` (unrelated to mahi) |
| `make check`                      | PASS — 1272 tests run, 1272 passed, 73 skipped |
| `make audit`                      | 4 known allowlisted (`RUSTSEC-2023-0071` / `RUSTSEC-2024-0436` / `RUSTSEC-2025-0134`; note: `RUSTSEC-2026-0009` listed in CLAUDE.md did not appear in current run). Additional advisories surfaced (`RUSTSEC-2025-0009`, `RUSTSEC-2025-0010`, `RUSTSEC-2026-0049`, `RUSTSEC-2026-0097`, `RUSTSEC-2026-0098`, `RUSTSEC-2026-0099`) are workspace-wide transitive issues (rand, axum, tungstenite, etc.) not introduced by the mahi conversion. Not a mahi blocker. |

## Open Follow-ups

| ID                  | Title | Priority |
|---------------------|-------|----------|
| `monitor-reef-xhec` | mahi: verify wire format of `SigV4VerifyResult.signingKey` | P2 |
| `monitor-reef-rqs1` | mahi: verify `GET /users?fallback` on-wire format and omission vs null user | P2 |

Both are labeled `restify-conversion` and are follow-ups to the "open
questions" already captured in plan.md. Neither blocks the API/client/CLI
conversion from shipping.

## Items Intentionally Not Filed

Per instructions:

- **No Rust mahi-server implementation** — orchestration-level decision,
  not a conversion gap. Plan.md documents the service-layer quirks that
  an eventual Rust server would need to honour.
- **Workspace-wide audit advisories** — transitive dep vulnerabilities
  independent of the mahi conversion.

## Overall Verdict

**READY TO SHIP.**

The mahi public API trait (`MahiApi`, 26 endpoints) and the internal
sitter API trait (`MahiSitterApi`, 2 endpoints) have been generated,
patched, client-generated, and wired into `tritonadm` with full endpoint
coverage. Every wire-format quirk called out in Phase 1 has a matching
patch and CLI handler. Type-safety rules hold. `make check` is green.

The two beads follow-ups are pure wire-format verification tasks that can
be resolved against a running mahi instance or by consulting
`test/integration/sigv4-sts-flow.test.js` in the upstream source; they do
not block consumers of the API / client / CLI.
