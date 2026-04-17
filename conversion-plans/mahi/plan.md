<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Mahi API Conversion Plan

## Source

- Path: `/Users/nshalman/Workspace/monitor-reef/target/mahi`
- Version: `2.1.0` (package.json)
- Package name: `mahi` ("Manta Auth Cache")
- License: MPL-2.0

Mahi is a Redis-backed auth cache that mirrors account/user/role/policy data
from UFDS and exposes lookup and AWS STS/IAM management endpoints. The code
ships **two independent Restify servers** in a single repo:

1. `lib/server/server.js` — the public **`mahi`** service (version `1.0.0` in
   restify metadata) that CloudAPI/manta/node-mahi clients call on port 8080.
   This is the server this plan targets.
2. `lib/replicator/server.js` — a separate internal **`mahi-sitter`** admin
   server run alongside the replicator process for health and snapshot
   shipping.

Everything under `v1/` is a legacy in-process replicator library (not an HTTP
server); `v1/main.js` starts it, `v1/lib/` holds transform modules. No HTTP
routes are defined there. Similarly, `lib/replicator/main.js` only stands up
the `mahi-sitter` server via `Server.createServer`. The only HTTP endpoints
in this repo are the 28 routes enumerated below.

Both servers are restify 2.6.1 and follow the vmapi-style `server.get({...},
handler)` pattern. The main server uses `restify.queryParser()` +
`restify.bodyParser()` with default options (which means `mapParams: true`,
i.e. query and body fields are merged into `req.params`). Handlers that read
`req.query.X` or `req.body.X` explicitly bypass the merged view.

## Endpoints Summary

- Total endpoints: **28** (26 on `mahi`, 2 on `mahi-sitter`)
- By method: GET: 17, POST: 9, DEL: 2
- Source files: `lib/server/server.js`, `lib/replicator/server.js`
- Groups (proposed modules):
  - `lookup` — classic auth-cache lookup routes (`/accounts`, `/users`,
    `/roles`, `/uuids`, `/names`, `/lookup`, `/ping`)
  - `lookup_deprecated` — legacy paths retained for backward compat
    (`/account/...`, `/user/...`, `POST /getUuid`, `POST /getName`)
  - `aws_sigv4` — `GET /aws-auth/{key}` and `POST /aws-verify`
  - `sts` — AWS STS actions (`/sts/*`)
  - `iam` — AWS IAM role/policy management (`/iam/*`)
  - `sitter` — replicator admin (`/ping`, `/snapshot`)

## Endpoints Detail

### Main mahi server (`lib/server/server.js`)

#### Lookup endpoints (classic, used by node-mahi client)

| Method | Path                  | Handler name                                                | Request                                                                             | Response                                       | Notes |
|--------|-----------------------|-------------------------------------------------------------|-------------------------------------------------------------------------------------|------------------------------------------------|-------|
| GET    | `/accounts/:accountid` | `getAccountByUuid`                                          | Path: `accountid`                                                                   | `AuthInfo { account, roles }`                  | Uuid lookup of an account; adds its roles. |
| GET    | `/accounts`           | `getAccount`                                                | Query: `login` (alias `account`)                                                    | `AuthInfo { account, roles }`                  | Login -> uuid -> account. |
| GET    | `/users/:userid`      | `getUserByUuid`                                             | Path: `userid`                                                                      | `AuthInfo { account, user, roles }`            | Uuid lookup of a sub-user; loads owning account. |
| GET    | `/users`              | `getUser`                                                   | Query: `account`, `login`, `fallback` (string `"true"`/`"false"`, default `"true"`) | `AuthInfo` (or `{account, roles}` when `fallback=true` and user not found) | Account-login + user-login -> user. `fallback=true` swallows missing-user errors and returns the account-only payload with HTTP 200. Handler side-effects make this a conditional response shape. |
| GET    | `/roles`              | `getRoleMembers`                                            | Query: `account`, `role` (alias `name`)                                             | `AuthInfo { account, role: { ..., members } }` | Lists members of a role. If role is missing the handler falls through with no response object (buggy in upstream, preserve behavior). |
| GET    | `/uuids`              | `nameToUuid`                                                | Query: `account`, `type` (`role`/`user`/`policy`), `name` (repeated)                | `NameToUuidResponse { account, uuids? }`       | Returns `{account}` alone if no `name`; else `{account, uuids: { name: uuid, ... }}`. |
| GET    | `/names`              | `uuidToName`                                                | Query: `uuid` (repeated)                                                            | `HashMap<String, String>` keyed by uuid, values are name-or-login | **Response is a map, not an array.** |
| GET    | `/ping`               | `ping`                                                      | —                                                                                   | 204 No Content on success                      | Errors 503 `RedisError` / `ReplicatorNotReady`. Needs `HttpResponseUpdatedNoContent`. |
| GET    | `/lookup`             | `lookup`                                                    | —                                                                                   | `HashMap<String, LookupEntry>` keyed by account uuid with `{approved, login}` | **Response is a map, not an array.** |

#### Deprecated lookup endpoints (kept for node-mahi v1 clients)

| Method | Path                    | Handler name | Request | Response | Notes |
|--------|-------------------------|--------------|---------|----------|-------|
| GET    | `/account/:account`     | `getAccountOld` | Path: `account` (login) | `AuthInfo { account }` (no roles) | Deprecated alias for `/accounts?login=`. |
| GET    | `/user/:account/:user`  | `getUserOld`   | Path: `account`, `user` | `AuthInfo { account, user, roles }` | Deprecated alias for `/users?account=&login=`. |
| POST   | `/getUuid`              | `nameToUuidOld` | Body: `account`, `type`, `name` (string or array) | Same as `/uuids` | Uses POST body; `mapParams: true` merges body into `req.params`. |
| POST   | `/getName`              | `uuidToNameOld` | Body: `uuid` (string or array) | Same as `/names` (map) | Uses POST body; same shape as GET. |

#### AWS SigV4 endpoints

| Method | Path                    | Handler name              | Request | Response | Notes |
|--------|-------------------------|---------------------------|---------|----------|-------|
| GET    | `/aws-auth/:accesskeyid` | `getUserByAccessKey`     | Path: `accesskeyid` | `AwsAuthResult { account, user, roles, assumedRole, isTemporaryCredential?, sessionName? }` | Redis first, falls back to UFDS for temporary (MSAR/MSTS) credentials. User not found returns 404 `ObjectDoesNotExist`. Temporary credentials longer than 16 chars get the UFDS fallback path. |
| POST   | `/aws-verify`            | `verifySigV4`            | Body: (caller supplies headers + query merged), reads `req.headers`, `req.query.method`, `req.query.url` | `SigV4VerifyResult { valid: true, accessKeyId, userUuid, assumedRole, principalUuid, isTemporaryCredential, signingKey }` | The endpoint forwards the entire request to `sigv4.verifySigV4` which re-derives a canonical SigV4 signature over the **original** HTTP request. The original method/URL are passed in `?method=...&url=...`. Keep this as an opaque passthrough — see "Special Handling" below. |

#### AWS STS endpoints (manta-only; 501 on sdc)

Gated by `ensureMantaInstance` middleware (returns `501 { error: "NotImplemented", message }` on sdc). All three accept a JSON body containing a `caller` object.

| Method | Path                          | Handler name            | Request body                                                                 | Response | Notes |
|--------|-------------------------------|-------------------------|-------------------------------------------------------------------------------|----------|-------|
| POST   | `/sts/assume-role`            | `stsAssumeRole`         | `{ caller: {account, user?}, RoleArn, RoleSessionName, DurationSeconds? }`    | `AssumeRoleResponse` nested under `{ AssumeRoleResponse: { AssumeRoleResult: { Credentials, AssumedRoleUser } } }` (JSON, **not XML**) | Validates trust policy, mints MSAR-prefixed temp creds. |
| POST   | `/sts/get-session-token`      | `stsGetSessionToken`    | `{ caller, DurationSeconds? }`                                                | `{ GetSessionTokenResponse: { GetSessionTokenResult: { Credentials } } }` | MSTS-prefixed temp creds. |
| POST   | `/sts/get-caller-identity`    | `stsGetCallerIdentity`  | `{ caller }` (also reads `x-assumed-role-arn` / `x-is-temporary-credential` headers) | **XML body** with `Content-Type: text/xml`, wire shape `<GetCallerIdentityResponse>...</GetCallerIdentityResponse>` | Only endpoint that returns non-JSON. See "Patch Requirements". |

#### AWS IAM endpoints (manta-only; 501 on sdc)

| Method | Path                                              | Handler name            | Request                                                                                              | Response | Notes |
|--------|---------------------------------------------------|-------------------------|------------------------------------------------------------------------------------------------------|----------|-------|
| POST   | `/iam/create-role`                                | `iamCreateRole`         | Body: `roleName`, `accountUuid`, `assumeRolePolicyDocument?`, `description?`, `path?` (default `"/"`) | 200 `{ Role: IamRole }` (despite docstring saying 201) | Writes Redis first, UFDS async. 409 `EntityAlreadyExists` if role exists. |
| GET    | `/iam/get-role/:roleName`                         | `iamGetRole`            | Path: `roleName`; Query: `accountUuid`                                                               | 200 `{ Role: IamRole }`                                           | 404 `NoSuchEntity` if missing. |
| POST   | `/iam/put-role-policy`                            | `iamPutRolePolicy`      | Body: `roleName`, `policyName`, `policyDocument` (JSON string), `mantaPolicy: { id, name, rules }`, `accountUuid` | 200 `{ message, roleName, policyName }`                                           | Writes full `mantaPolicy` object under `/policy/:id`; appends entry to `/role-permissions/:uuid`. |
| DEL    | `/iam/delete-role/:roleName`                      | `iamDeleteRole`         | Path: `roleName`; Query: `accountUuid`                                                               | 200 `{ message, roleName }`                                           | Deletes Redis synchronously, UFDS async. |
| DEL    | `/iam/delete-role-policy`                         | `iamDeleteRolePolicy`   | Query: `roleName`, `policyName`, `accountUuid`                                                       | 200 `{ message, roleName, policyName }`                                           | All identifiers in query string, not path. |
| GET    | `/iam/list-roles`                                 | `iamListRoles`          | Query: `accountUuid`, `maxItems?` (default 100), `marker?` (aka `startingToken?`)                    | 200 `ListRolesResponse { roles: IamRole[], IsTruncated: bool, Marker: string\|null }` | **Top-level field casing is mixed**: `roles` lowercase, `IsTruncated`/`Marker` PascalCase. |
| GET    | `/iam/list-role-policies/:roleName`               | `listRolePolicies`      | Path: `roleName`; Query: `accountUuid`, `marker?`, `maxitems?` (default 100) **(lowercase!)**        | 200 `ListRolePoliciesResponse { PolicyNames: string[], IsTruncated, Marker }` | Note `maxitems` vs `maxItems` on list-roles — inconsistent in upstream. |
| GET    | `/iam/get-role-policy/:roleName/:policyName`      | `getRolePolicy`         | Path: `roleName`, `policyName`; Query: `accountUuid`                                                 | 200 `{ RoleName, PolicyName, PolicyDocument }` (PolicyDocument is a JSON string) | 404 if policy not attached. |

### Replicator sitter server (`lib/replicator/server.js`)

Separate service on its own port; proposed separate API crate `mahi-sitter-api` OR a gated `sitter` module in `mahi-api`. Recommendation: keep it in the same crate under a `sitter` module and tag endpoints with `tags = ["sitter"]`.

| Method | Path        | Handler  | Request | Response | Notes |
|--------|-------------|----------|---------|----------|-------|
| GET    | `/ping`     | `ping`   | —       | 204 No Content on success | 500 `RedisUnavailable`/`RedisError`, 503 `NotCaughtUp`. |
| GET    | `/snapshot` | `snapshot` | —     | 201 with streamed Redis `dump.rdb` binary body | Binary streaming endpoint. Sends `res.send(201)` **after** piping the file into `res` — Restify quirk. See "Patch Requirements". |

## Route Conflicts

No conflicts that require Dropshot-level resolution. In particular:

- `/iam/get-role/{roleName}`, `/iam/delete-role/{roleName}`,
  `/iam/list-role-policies/{roleName}`, and
  `/iam/get-role-policy/{roleName}/{policyName}` all live under different
  literal prefixes (`get-role`, `delete-role`, ...), so no segment mixes a
  literal with a variable at the same depth.
- `/accounts` and `/accounts/{accountid}` live at different depths (root
  collection vs uuid resource) — Dropshot handles this.
- `/ping` exists in both servers but on separate ports; no cross-service
  conflict.

**Status: RESOLVED** (no action required).

## Action Dispatch Endpoints

None. Mahi does not use the single-endpoint/action-query pattern. STS and IAM
operations each have their own path and their own typed request body.

## Planned File Structure

```
apis/mahi-api/src/
├── lib.rs                 # trait MahiApi + re-exports
├── types/
│   ├── mod.rs             # common re-exports + Uuid alias
│   ├── common.rs          # AuthInfo, Account, User, Role, Policy, Rule
│   ├── accesskey.rs       # AwsAuthResult, AccessKeySecret, AssumedRole
│   ├── sigv4.rs           # SigV4VerifyResult
│   ├── sts.rs             # AssumeRoleRequest/Response, GetSessionToken*, Caller
│   ├── iam.rs             # IamRole, CreateRoleRequest, PutRolePolicyRequest, ListRolesResponse, ...
│   └── lookup.rs          # NameToUuidResponse, UuidToNameResponse, LookupEntry
├── lookup.rs              # GET /accounts, /users, /roles, /uuids, /names, /ping, /lookup + deprecated POSTs
├── aws_sigv4.rs           # GET /aws-auth/{id}, POST /aws-verify
├── sts.rs                 # POST /sts/*
├── iam.rs                 # /iam/* (GET/POST/DEL)
└── sitter.rs              # separate MahiSitterApi trait for /ping + /snapshot
```

Rationale:
- 28 endpoints in one trait would be unwieldy; split by logical group mirrors
  the source file's section layout.
- The sitter service runs on a different port and has a completely separate
  consumer (replicator operator) — keep it as a second trait in the same
  crate so we can still share error types / util code.

## Enum Opportunities

1. **`ObjectType`** — used by `GET /uuids?type=`. Fixed set: `role`, `user`,
   `policy`. Asserted in `redislib.getUuid` via
   `['role', 'user', 'policy'].indexOf(opts.type) >= 0`. Wire format is
   lowercase.

2. **`ObjectTypeTag`** — the internal `type` field stored on Redis blobs
   (`account`, `user`, `role`, `policy`, `accesskey`). Surfaces in
   `AuthInfo.account.type`, `AuthInfo.user.type`, etc. Needs
   `#[serde(other)] Unknown` because Mahi may grow additional object types.

3. **`CredentialType`** — on access-key records: `permanent` vs `temporary`
   (wire values `"permanent"` and `"temporary"`). Appears in
   `AccessKeySecret.type` and `AwsAuthResult.*.accesskeys[...].type`. Needs
   `Unknown` variant for forward compat.

4. **`CredentialStatus`** — LDAP `status` field on access keys. Observed
   values: `"Active"` (set by `sts.js`). Document but prefer `String` until
   full set is known (values come from UFDS).

5. **`ArnPartition`** — `DEFAULT_ARN_PARTITION` accepts `aws`, `manta`,
   `triton`. Used by `/sts/get-caller-identity`. The ARN regex in
   `validateStsAssumeRoleInputs` enforces the same set. Wire values are
   lowercase.

6. **`InstanceFlavor`** — `manta` vs `sdc`. Not on the wire (set via
   `mdata-get` in `initializeInstanceFlavor`) — internal Rust enum only, not
   an API type. Mentioned for completeness.

7. **No need for `MahiErrorCode`** — Restify `RestError.restCode` values
   (`AccountDoesNotExist`, `ObjectDoesNotExist`, `RedisError`,
   `ReplicatorNotReady`, `AccessKeyNotFound`, `InvalidSignature`,
   `RequestTimeTooSkewed`, `AccessDenied`, `InvalidParameterValue`,
   `NoSuchEntity`, `InternalError`, `ServiceUnavailable`) should become
   documented Dropshot `HttpError` causes, not a typed enum on the wire.

The `fallback` query parameter on `GET /users` is documented as a stringy
boolean (`"true"` / `"false"`). Model as `Option<bool>` with
`#[serde(default)]` and rely on serde's `Deserialize`. If the existing
clients send the literal string `"true"`, we may need a patched
`StringifiedBool` type — verify during Phase 5.

## Response-Pattern Catalog (Restify Wire Quirks)

For each endpoint, here is the exact `res.send(...)` pattern(s):

| Endpoint                               | Restify call(s)                                    | Dropshot mapping                                              |
|----------------------------------------|---------------------------------------------------|---------------------------------------------------------------|
| `GET /accounts/{id}`                   | `res.send(req.auth)` (via `sendAuth`)             | `HttpResponseOk<AuthInfo>`                                    |
| `GET /accounts`                        | `res.send(req.auth)`                              | `HttpResponseOk<AuthInfo>`                                    |
| `GET /users/{id}`                      | `res.send(req.auth)`                              | `HttpResponseOk<AuthInfo>`                                    |
| `GET /users`                           | `res.send(req.auth)` OR (fallback) `res.send(req.auth)` with partial auth | `HttpResponseOk<AuthInfo>` — fallback still sends the same type, partially populated |
| `GET /roles`                           | `res.send(req.auth)` — but if role missing, no response is sent at all (upstream bug, results in hang/timeout). Should patch to return `404` on missing role. | `HttpResponseOk<AuthInfo>` + **spec patch/service fix**: return 404 on missing role. |
| `GET /uuids`                           | `res.send(body)`                                  | `HttpResponseOk<NameToUuidResponse>`                          |
| `GET /names`                           | `res.send(body)` (body is an object keyed by uuid) | `HttpResponseOk<HashMap<String,String>>`                      |
| `GET /ping`                            | `res.send(204)` on success                        | `HttpResponseUpdatedNoContent`                                |
| `GET /lookup`                          | `res.send(lookup)` (map keyed by account uuid)    | `HttpResponseOk<HashMap<String, LookupEntry>>`                |
| `GET /account/{account}`               | `res.send(req.auth)`                              | `HttpResponseOk<AuthInfo>`                                    |
| `GET /user/{account}/{user}`           | `res.send(req.auth)`                              | `HttpResponseOk<AuthInfo>`                                    |
| `POST /getUuid`                        | Same as `GET /uuids`                              | `HttpResponseOk<NameToUuidResponse>`                          |
| `POST /getName`                        | Same as `GET /names`                              | `HttpResponseOk<HashMap<String,String>>`                      |
| `GET /aws-auth/{id}`                   | `res.send(authResult)`                            | `HttpResponseOk<AwsAuthResult>`                               |
| `POST /aws-verify`                     | `res.send({valid, ...})`                          | `HttpResponseOk<SigV4VerifyResult>`                           |
| `POST /sts/assume-role`                | `res.send(200, response)` — nested JSON (not XML despite the AWS API it emulates) | `HttpResponseOk<AssumeRoleResponse>` |
| `POST /sts/get-session-token`          | `res.send(200, response)` — nested JSON           | `HttpResponseOk<GetSessionTokenResponse>`                     |
| `POST /sts/get-caller-identity`        | `res.send(200, responseXml)` with `Content-Type: text/xml` | **Patch required**: return `Result<Response<Body>, HttpError>`. Client consumes opaque bytes. |
| `POST /iam/create-role`                | `res.send(200, response)` — nb. spec docstring says 201, code sends 200 | `HttpResponseOk<CreateRoleResponse>` (match the actual 200) |
| `GET /iam/get-role/{roleName}`         | `res.send(200, response)`                         | `HttpResponseOk<GetRoleResponse>`                             |
| `POST /iam/put-role-policy`            | `res.send(200, {...})`                            | `HttpResponseOk<PutRolePolicyResponse>`                       |
| `DEL /iam/delete-role/{roleName}`      | `res.send(200, {...})`                            | `HttpResponseOk<DeleteRoleResponse>`                          |
| `DEL /iam/delete-role-policy`          | `res.send(200, {...})`                            | `HttpResponseOk<DeleteRolePolicyResponse>`                    |
| `GET /iam/list-roles`                  | `res.send(200, {roles, IsTruncated, Marker})`     | `HttpResponseOk<ListRolesResponse>`                           |
| `GET /iam/list-role-policies/{role}`   | `res.send(200, {PolicyNames, IsTruncated, Marker})` | `HttpResponseOk<ListRolePoliciesResponse>`                  |
| `GET /iam/get-role-policy/{r}/{p}`     | `res.send(200, {RoleName, PolicyName, PolicyDocument})` | `HttpResponseOk<GetRolePolicyResponse>`                |
| `GET /ping` (sitter)                   | `res.send(204)`                                   | `HttpResponseUpdatedNoContent`                                |
| `GET /snapshot` (sitter)               | `stream.pipe(res)` + `res.send(201)` at end       | **Patch required**: `Result<Response<Body>, HttpError>` with `application/octet-stream`. |

## Patch Requirements

The following endpoints will need post-generation OpenAPI spec patches or
special Dropshot handling:

1. **`POST /sts/get-caller-identity` — XML body**  
   Emits raw XML with `Content-Type: text/xml`. Use
   `Result<Response<Body>, HttpError>` in the trait and patch the response
   content in the spec to `{"content": {"text/xml": {"schema": {"type":
   "string"}}}}`. Clients will receive the XML as a string.

2. **`GET /snapshot` (sitter) — binary streaming body**  
   Streams `dump.rdb` bytes to the client and sends status 201 after. Use
   `Result<Response<Body>, HttpError>` and patch the spec to
   `{"application/octet-stream": {"schema": {"type": "string", "format":
   "binary"}}}`. Document the `201 Created` status.

3. **`GET /ping` and sitter `GET /ping` — 204 with no body**  
   Use `HttpResponseUpdatedNoContent`. No patch required (Dropshot renders
   204 natively).

4. **`POST /iam/create-role` — docstring says 201, code sends 200**  
   Docstrings in the JSDoc claim `returns 201`. The actual handler calls
   `res.send(200, response)` twice (both branches). Match the wire behavior
   (200) in the trait; do not patch the spec.

5. **`GET /users?fallback=true` — shape-varying response**  
   When a sub-user doesn't exist and `fallback=true`, the handler returns
   `AuthInfo` populated with only the account (no `user`, no `roles`). The
   `user` field on `AuthInfo` must be `Option<User>`, and `roles` must
   default to empty map — this is a type-level accommodation, not a spec
   patch.

6. **`GET /roles` — missing role produces no response**  
   Upstream handler calls `next()` without sending a response if the role
   isn't found. This deadlocks restify request handling in practice.
   **Recommendation**: the Rust service should return a proper `404` with
   restCode `RoleDoesNotExist`. This is a documented behavior fix, not a
   bug-for-bug migration.

7. **`POST /aws-verify` — re-signs the original request**  
   The handler reconstructs a canonical SigV4 signature over the original
   HTTP request, using `?method=` and `?url=` query params plus the full
   forwarded headers. The request body is effectively opaque. The API trait
   should accept `TypedBody<serde_json::Value>` for the request body (which
   is ignored) and a `Query<SigV4VerifyQuery { method: String, url: String
   }>`. The service implementation will need access to `req.headers()` —
   document this as a non-standard handler requirement. The response shape
   is well-defined and doesn't need a patch.

8. **SigV4 endpoint needs raw headers** — unlike typical Dropshot endpoints,
   `/aws-verify` inspects incoming headers like `Authorization`,
   `x-amz-date`, `x-amz-content-sha256`, `x-amz-security-token`. Dropshot
   provides header access via `RequestContext::request`. No spec patch, but
   flag for Phase 2 implementation.

## Types to Define

### Shared (`types/common.rs`)

- `type Uuid = uuid::Uuid;` (alias) — follows repo convention.
- `ObjectType` enum: `Role`, `User`, `Policy` (lowercase wire).
- `ObjectTypeTag` enum: `Account`, `User`, `Role`, `Policy`, `AccessKey`,
  `#[serde(other)] Unknown`.
- `CredentialType` enum: `Permanent`, `Temporary`, `#[serde(other)] Unknown`.
- `Account { uuid, login, type, approved_for_provisioning, isOperator,
  groups?, roles?, keys?, email?, cn?, company?, s?, phone?, address?,
  country?, postal_code?, state?, city?, given_name?, sn?,
  triton_cns_enabled? }` — modeled against the translate() and the tests.
  Many fields are optional because they come from UFDS. Add
  `#[serde(flatten)] extra: serde_json::Value` to preserve passthrough
  fields we don't explicitly model.
- `User { uuid, login, account, type, roles?, accesskeys?, email?, cn?,
  company?, s?, given_name?, sn? }`.
- `Role { uuid, name, account, type, policies, rules?,
  assumerolepolicydocument?, createtime?, path?, description?,
  permissionPolicies? }`.
- `Policy { uuid, name, account, type, rules }`.
- `AccessKeySecret { secret: String, type: CredentialType,
  expiration: Option<String>, sessionToken: Option<String>,
  principalUuid: Option<Uuid>, assumedRole: Option<serde_json::Value> }` —
  the inner value of `user.accesskeys[keyId]` after temp-credential
  injection.
- `AuthInfo { account: Account, user: Option<User>,
  roles: HashMap<String, Role>, role: Option<Role> /* for /roles */ }`.

### Lookup (`types/lookup.rs`)

- `NameToUuidResponse { account: Uuid, uuids: Option<HashMap<String, Uuid>> }`.
- `LookupEntry { approved: bool, login: String }`.
- Query structs: `GetAccountQuery { login: Option<String> }`,
  `GetUserQuery { account: String, login: String, fallback: Option<bool> }`,
  `GetRolesQuery { account: String, role: Option<String>, name: Option<String> }`,
  `NameToUuidQuery { account: String, r#type: ObjectType, name: Vec<String> }`,
  `UuidToNameQuery { uuid: Vec<Uuid> }`.
- Body structs for deprecated POSTs: `NameToUuidBody`, `UuidToNameBody` with
  matching fields; accept single string or array via
  `#[serde(default)] #[serde(with = "string_or_vec")]` helper.

### SigV4 / AWS auth (`types/sigv4.rs`, `types/accesskey.rs`)

- `AwsAuthResult { account: Account, user: Option<User>,
  roles: HashMap<String, Role>, assumedRole: Option<AssumedRole>,
  isTemporaryCredential: Option<bool>, sessionName: Option<String> }`.
- `AssumedRole { arn: String, sessionName: Option<String>,
  roleUuid: Option<Uuid>, policies: Option<Vec<PolicyEntry>> }` (see
  `getUserByAccessKey`).
- `SigV4VerifyQuery { method: String, url: String }`.
- `SigV4VerifyResult { valid: bool, accessKeyId: String, userUuid: Uuid,
  assumedRole: Option<serde_json::Value>, principalUuid: Option<Uuid>,
  isTemporaryCredential: Option<bool>, signingKey: Option<Vec<u8>> /* or hex */ }`.
  `signingKey` comes from `calculateSignature` — it's a `Buffer` on the
  wire (JSON would produce `[n, n, ...]`). Verify with
  `test/integration/sigv4-sts-flow.test.js` during Phase 5.

### STS (`types/sts.rs`)

- `Caller { account: CallerAccount, user: Option<CallerUser> }` where both
  sub-structs have `{ uuid: Uuid, login: String }` plus optional fields.
- `AssumeRoleRequest { caller: Caller, RoleArn: String,
  RoleSessionName: String, DurationSeconds: Option<u64> /* 900..=43200 */ }`.
  Note the PascalCase field names are AWS convention — use
  `#[serde(rename = "RoleArn")]` etc. on snake_case Rust fields.
- `AssumeRoleResponse { AssumeRoleResponse: AssumeRoleResponseInner }`
  (double-wrapped, matches source). The inner types:
  `Credentials { AccessKeyId, SecretAccessKey, SessionToken, Expiration }`,
  `AssumedRoleUser { AssumedRoleId, Arn }`.
- `GetSessionTokenRequest { caller: Caller, DurationSeconds: Option<u64> /* 900..=129600 */ }`.
- `GetSessionTokenResponse { GetSessionTokenResponse: ... }`.
- `GetCallerIdentityRequest { caller: Caller }` — and the **response is XML**
  (see Patch Requirements).

### IAM (`types/iam.rs`)

- `IamRole { Path, RoleName, RoleId, Arn, CreateDate, AssumeRolePolicyDocument,
  Description, MaxSessionDuration }` — all PascalCase, matches AWS IAM.
- `CreateRoleRequest { roleName, accountUuid: Uuid,
  assumeRolePolicyDocument: Option<String>, description: Option<String>,
  path: Option<String> }` — mixed casing: request is camelCase, response is
  PascalCase (nested in `Role`).
- `CreateRoleResponse { Role: IamRole }`.
- `GetRoleQuery { accountUuid: Uuid }`, `DeleteRoleQuery { accountUuid: Uuid }`.
- `PutRolePolicyRequest { roleName, policyName, policyDocument: String,
  mantaPolicy: MantaPolicy, accountUuid: Uuid }`.
- `MantaPolicy { id: Uuid, name: String, rules: Vec<String> }` — stored
  under `/policy/:id` in Redis. Matches the `Policy` struct pattern.
- `PutRolePolicyResponse { message, roleName, policyName }`.
- `DeleteRoleResponse { message, roleName }`.
- `DeleteRolePolicyQuery { roleName, policyName, accountUuid: Uuid }`.
- `DeleteRolePolicyResponse { message, roleName, policyName }`.
- `ListRolesQuery { accountUuid: Uuid, maxItems: Option<u32> /* cap 1000 */,
  marker: Option<String>, startingToken: Option<String> }`.
- `ListRolesResponse { roles: Vec<IamRole>, IsTruncated: bool,
  Marker: Option<String> }` — **mixed casing** (see Field Naming Exceptions).
- `ListRolePoliciesQuery { accountUuid: Uuid, marker: Option<String>,
  maxitems: Option<u32> /* lowercase! */ }`.
- `ListRolePoliciesResponse { PolicyNames: Vec<String>, IsTruncated: bool,
  Marker: Option<String> }`.
- `GetRolePolicyQuery { accountUuid: Uuid }`.
- `GetRolePolicyResponse { RoleName, PolicyName, PolicyDocument: String }`.

## Field Naming Exceptions

`translate()` is not a single function in this codebase — each handler
builds its own response inline. Grep of `res.send(...)` call sites plus
`redis.get()` of blobs tells us the following:

### Passthrough (snake_case) fields

Redis blobs come straight from the replicator, which mirrors UFDS attribute
names verbatim. The following fields on `Account`, `User`, `Role`, `Policy`
use **snake_case on the wire**:

- `approved_for_provisioning` (Account)
- `isOperator` (Account) — **camelCase**, set by `redislib.getAccount`
- `triton_cns_enabled` (Account, UFDS pass-through; mentioned in MEMORY.md)
- `given_name`, `sn`, `cn`, `s`, `company`, `postal_code` (Account/User)
- `assumerolepolicydocument`, `createtime`, `permissionPolicies` (Role —
  `permissionPolicies` is camelCase, others are all-lowercase)
- `accesskeyid`, `accesskeysecret`, `sessiontoken`, `principaluuid`,
  `credentialtype`, `assumedrole` (UFDS-level access-key attrs — all
  lowercase)

### Explicit PascalCase AWS fields (no rename_all works)

The STS/IAM subset uses AWS naming which mixes PascalCase containers,
camelCase containers, and lowercase field names:

- `IamRole`: Every field is PascalCase (`RoleName`, `RoleId`, `Arn`,
  `CreateDate`, `AssumeRolePolicyDocument`, `Description`, `Path`,
  `MaxSessionDuration`). Use `#[serde(rename_all = "PascalCase")]` on the
  struct.
- Outer STS wrappers are nested PascalCase:
  `{ AssumeRoleResponse: { AssumeRoleResult: { Credentials: { AccessKeyId,
   ... }, AssumedRoleUser: { ... } } } }`.
- Request bodies are lowerCamel (`roleName`, `accountUuid`,
  `policyDocument`) — use `#[serde(rename_all = "camelCase")]`.
- Pagination fields on list responses are inconsistent:
  `ListRolesResponse` has `roles` (lowercase) + `IsTruncated`/`Marker`
  (PascalCase). Apply **field-level** `#[serde(rename = "IsTruncated")]`
  rather than struct-level `rename_all`.

### Query-parameter casing

- `/iam/list-roles`: `accountUuid` (camel), `maxItems` (camel),
  `marker`/`startingToken` (camel/camelCase)
- `/iam/list-role-policies`: `accountUuid` (camel), `marker`,
  `maxitems` (**lowercase**, no 'I')

This `maxItems` vs `maxitems` inconsistency is a real upstream bug; the
Rust crate should accept both spellings on `list-role-policies` to preserve
compatibility, via a field with
`#[serde(alias = "maxItems")] maxitems: Option<u32>`.

## WebSocket / Channel Endpoints

**None.** No `ws.on`, no upgrade handling, no SSE. All endpoints are
request/response. The `/snapshot` endpoint is HTTP response streaming
(binary body), not a WebSocket.

## Non-Restify / Non-HTTP Concerns

Files that are **not** HTTP handlers (for completeness, so Phase 2 knows to
skip them):

- `lib/replicator/**` — LDAP changelog watcher + Redis transforms. No HTTP.
  The `replicator/server.js` **is** an HTTP service (the sitter above),
  everything else under `replicator/` is background logic.
- `lib/server/sts.js`, `lib/server/sigv4.js`, `lib/server/session-token.js`,
  `lib/server/redislib.js`, `lib/server/utils.js`, `lib/server/errors.js` —
  helper libraries called by the route handlers in `server.js`.
- `v1/*` — legacy auth-cache library, in-process replicator, not HTTP.
- `bin/*`, `tools/*`, `boot/*`, `etc/*`, `smf/*`, `sapi_manifests/*` — zone
  boot/admin scripts.
- `test/**` — nodeunit tests; useful Phase 5 reference for wire shapes.

## Open Questions / Things to Verify in Phase 5

1. Does `GET /users?fallback=true` on a missing user really return
   `AuthInfo` with `user: null`, or does it omit the key entirely? Verify
   via a fixture test.
2. Wire format of `SigV4VerifyResult.signingKey` — Node Buffer JSON-encodes
   as `{"type":"Buffer","data":[...]}`. Check what callers expect.
3. Whether `fallback` is sent as string `"true"` or boolean `true` by real
   clients (node-mahi).
4. Whether the broken `GET /roles` (missing role, no response) is relied
   upon by any caller. If not, return 404 as recommended.
5. Whether the `Role.permissionPolicies` field is serialized on responses
   or only used internally in STS evaluation.

## Phase Status

- [x] Phase 1: Analyze — **COMPLETE**
- [ ] Phase 2: Generate API
- [ ] Phase 3: Generate Client
- [ ] Phase 4: Generate CLI
- [ ] Phase 5: Validate
