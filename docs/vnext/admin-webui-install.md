<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Installing the vNext admin webui on a SmartOS box

A field-tested runbook for getting the admin webui live against a
`tritond` you already have running. The actual webui is a published
illumos binary on the Manta release channel — there is no `admin-backend`
crate in this monorepo today (only SMF scaffolding in
`images/triton-tritond/proto/opt/triton/admin-backend/`).

The published binary is built from
[`mariana-trench/services/triton-admin/`](../../../mariana-trench/services/triton-admin/),
which embeds a Vite-built React SPA via `rust-embed` at compile time.

> [!IMPORTANT]
> The currently-published binary was built **before** commit
> [`a13a9889`](https://github.com/...) ("refactor(api): migrate /v2/ -> /v1/ for
> the workload + operator surfaces"). Every `tritond` build on this branch
> is post-`a13a9889`, so the binary lands in a path-mismatch with the API
> it talks to. See the [Login failure mode](#login-failure-mode) section.

## TL;DR

```sh
# On the test box (192.168.1.182, root):
export CURL_CA_BUNDLE=/opt/tools/etc/openssl/certs/ca-certificates.crt
BASE=https://us-central.manta.mnx.io/nick.wilkens@mnxsolutions.com/public/tritoncloud/sources

# 1. fetch + install the binary
mkdir -p /var/tmp/tritoncloud/sources /opt/triton-admin/bin
curl -fS --retry 5 -o /var/tmp/tritoncloud/sources/admin-backend-illumos.bin \
    "$BASE/admin-backend-illumos.bin"
mv /var/tmp/tritoncloud/sources/admin-backend-illumos.bin \
   /opt/triton-admin/bin/triton-admin
chmod 0755 /opt/triton-admin/bin/triton-admin

# 2. launch (binds 127.0.0.1:3030 regardless of env — see Gotcha #1)
nohup env ADMIN_BIND_ADDRESS=0.0.0.0:3030 \
    TRITOND_URL=http://127.0.0.1:8080 \
    RUST_LOG=info \
    /opt/triton-admin/bin/triton-admin \
    > /var/log/triton-admin.out 2>&1 < /dev/null &
disown

# 3. expose 0.0.0.0:3030 via socat (workaround for the ignored
#    ADMIN_BIND_ADDRESS env var; see Gotcha #1)
nohup /usr/bin/socat -d \
    TCP-LISTEN:3030,bind=192.168.1.182,reuseaddr,fork \
    TCP:127.0.0.1:3030 > /var/log/triton-admin-fwd.out 2>&1 &
disown

# 4. verify
curl -sS -w "%{http_code}\n" http://192.168.1.182:3030/        # 200 → SPA
curl -sS -w "%{http_code}\n" http://192.168.1.182:3030/api/me  # 401 → live
```

Then browse to <http://192.168.1.182:3030/> and log in as `root` with the
password printed at tritond's first-boot bootstrap. Recover it from
`/var/log/tritond.out` (look for the `tritond bootstrap: created root operator`
banner) or regenerate with:

```sh
LD_LIBRARY_PATH=/opt/fdb/lib \
    /opt/tritond/bin/tritond reset-root-password \
        --config /etc/tritond/config.toml
```

## What's running

| Process | Path | Port | Role |
|---|---|---|---|
| `triton-admin` | `/opt/triton-admin/bin/triton-admin` | `127.0.0.1:3030` | Axum BFF + embedded React SPA |
| `socat` | (transient) | `192.168.1.182:3030` | TCP forwarder → loopback |
| `tritond` | `/opt/tritond/bin/tritond` | `0.0.0.0:8080` | upstream API (this monorepo) |

The admin-backend talks to tritond over loopback. The SPA in the user's
browser only ever talks to the admin-backend; it doesn't reach tritond
directly.

## Gotchas

### Gotcha #1 — env vars are silently ignored

The published binary's string table contains `ADMIN_BIND_ADDRESS` and
`TRITOND_URL` literals, but the runtime ignores them. Startup always
binds `127.0.0.1:3030` and dials `http://127.0.0.1:8080`. The startup
log line confirms it:

```
admin-backend listening bind=127.0.0.1:3030 tritond_url=http://127.0.0.1:8080
```

**Workarounds:**

- **For external access (binding):** run `socat` as a forwarder
  (`TCP-LISTEN:3030,bind=<PUBLIC_IP>,reuseaddr,fork TCP:127.0.0.1:3030`).
  The two listens coexist because they don't overlap on the same
  address.
- **For upstream redirection:** there is no clean workaround. Tritond
  *must* bind `127.0.0.1:8080` or `0.0.0.0:8080`.

A future re-build of `mariana-trench/services/triton-admin/` from
current source should wire these env vars properly (the current source
tree even has a YAML config schema in
`mariana-trench/services/triton-admin/config/default.yaml` — also
ignored by the published binary).

### Gotcha #2 — login failure mode (post-`a13a9889`)

<a name="login-failure-mode"></a>

If you've already deployed and login returns

```json
{"error":"upstream tritond: 404 "}
```

that's not a password problem. It's a path-version mismatch.

**Diagnosis.** Reset the password to be sure, then probe both API
versions directly against tritond. If `/v1/auth/login` returns 200
but `/v2/auth/login` returns 404, you're hitting this bug:

```sh
# Both should return 200. Replace <pw> with the actual root password.
curl -sS -o /dev/null -w "/v1: %{http_code}\n" \
    -X POST -H "Content-Type: application/json" \
    -d '{"username":"root","password":"<pw>"}' \
    http://192.168.1.182:8080/v1/auth/login

curl -sS -o /dev/null -w "/v2: %{http_code}\n" \
    -X POST -H "Content-Type: application/json" \
    -d '{"username":"root","password":"<pw>"}' \
    http://192.168.1.182:8080/v2/auth/login
```

**Cause.** Commit `a13a9889` ("refactor(api): migrate /v2/ -> /v1/ for
the workload + operator surfaces", Nick Wilkens, 2026-05-27 11:56 EDT)
renamed 155 endpoint paths in `apis/tritond-api/src/lib.rs`. The
published admin-backend binary still issues `POST /v2/auth/login` as
the second leg of its login handshake.

**Fix (this branch).** Commit `2c13b6ba`
("fix(api): add /v2/auth/login alias for published admin-backend binary")
adds a sibling trait method `login_v2_alias` on the same handler as
`login`, mounted on `/v2/auth/login` with `unpublished = true` so the
OpenAPI spec and generated client stay clean. Net change: ~22 lines.

After deploying a tritond build that contains `2c13b6ba`, both paths
return 200 and login completes. Eight authenticated SPA endpoints
(`/api/me`, `/api/cns`, `/api/silos`, `/api/manta/clusters`,
`/api/migrations`, `/api/config`, `/api/audit/events`,
`/api/aggregate/instances`) verify clean post-login. The tritond log
shows zero further `/v2/*` hits from the admin-backend; `/v2/auth/login`
is the only v2-pinned upstream path in the published binary.

**If you hit a different `/v2/*` 404 post-login.** Tail
`/var/log/tritond.log` while exercising the UI; any
`response_code: 404, uri: /v2/...` entries are candidates. Add a
parallel `_v2_alias` trait method + `service_impl` delegate using
`2c13b6ba` as the template, rebuild, redeploy.

### Gotcha #3 — admin-backend negative-caches a failed login flow

Once login has 404'd on a process, that process can keep returning
404 with `latency=0 ms` (i.e. no upstream call) for some time even
after the upstream path is fixed. Restart `triton-admin` after the
tritond fix lands:

```sh
pkill -x triton-admin
nohup env ADMIN_BIND_ADDRESS=0.0.0.0:3030 \
    TRITOND_URL=http://192.168.1.182:8080 \
    RUST_LOG=info \
    /opt/triton-admin/bin/triton-admin \
    > /var/log/triton-admin.out 2>&1 < /dev/null &
```

### Gotcha #4 — auth is tritond's, not LDAP

The published binary uses tritond's own bcrypt-hashed operator
credentials. The `mariana-trench/services/triton-admin/config/default.yaml`
has an LDAP/UFDS block, but it's a leftover from the legacy SDC admin
BFF source — the *current* published binary doesn't load YAML config
and doesn't dial LDAP.

`tritond reset-root-password` (or recovering the bootstrap password
from `/var/log/tritond.out`) is how you log in. `tcadm api-key create`
after that mints long-lived API keys for non-interactive callers.

## Building from source

There's an option to rebuild from current source on `build02`:

```sh
# On build02 (or any illumos build host with rustup + libfdb_c):
rsync -az --exclude='target' --exclude='node_modules' --exclude='.git' \
    ~/Projects/Triton-S3/mariana-trench/ build@build02.local:~/mariana-trench/

ssh build@build02.local '
    cd ~/mariana-trench/services/triton-admin/frontend \
        && npm ci && npm run build
    cd ~/mariana-trench \
        && export PATH=$HOME/.rustup/toolchains/1.92-x86_64-unknown-illumos/bin:$PATH \
        && cargo build --release -p triton-admin
    strip ~/mariana-trench/target/release/triton-admin
'
```

That **will not** give you a working webui today — the current source
tree expects YAML config (LDAP, SAPI/VMAPI/CNAPI proxies, …) that
doesn't match tritond's surface. It's the legacy SDC admin BFF skeleton
that the published binary diverged from. A real fix is a new
`admin-backend` crate built against the current `tritond-client`.

## Cross-references

- [`operating-tritond.md`](./operating-tritond.md) — tritond bootstrap
  config + cluster settings.
- [`s3-data-plane-workspace-gate.md`](./s3-data-plane-workspace-gate.md) —
  Phase D admin-plane work that this UI session piggybacks on.
- [`~/notes/monitor_reef_installing_triton_cloud_vnext_on_smartos.org`](file:///Users/carlosneira/notes/monitor_reef_installing_triton_cloud_vnext_on_smartos.org) —
  the older org-roam runbook this doc was distilled from; carries
  the full SmartOS install context (FDB, tritond, channel manifests,
  CA bundle gotchas, zone-image origin gap).
- Commit [`a13a9889`](https://github.com/...) — the `/v2/* → /v1/*`
  migration that broke the published binary's login path.
- Commit [`2c13b6ba`](https://github.com/...) — the
  `/v2/auth/login` alias fix that re-unblocked login.
