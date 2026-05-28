# `tritonagent` agent tarball

Per-CN provisioning agent. Runs in the global zone of every CN
(controller and tenant). Talks to tritond at `$TRITONAGENT_ENDPOINT`,
drives `vmadm` for provisioning, plus the proteus port lifecycle,
edge cluster realization, on-CN console, and live-migration listeners.

## Layout (in the tarball, extracted at `/`)

```
/opt/triton/tritonagent/bin/tritonagent      release binary
/opt/triton/tritonagent/etc/agent.env        operator config (env=value lines)
/opt/triton/tritonagent/etc/agent.env.example  ditto, template
/opt/triton/tritonagent/etc/version          build stamp written at package time
/opt/triton/tritonagent/smf/tritonagent.xml  SMF manifest
/var/svc/method/tritonagent                  start method
```

## Install (manual)

```bash
# As root in the CN's global zone:
cd / && tar -xzf tritonagent-<stamp>.tar.gz
cat >/opt/triton/tritonagent/etc/agent.env <<'EOF'
TRITONAGENT_ENDPOINT=http://172.16.96.4:8080
EOF
svccfg import /opt/triton/tritonagent/smf/tritonagent.xml
svcadm enable -s site/tritonagent
svcs -p site/tritonagent
```

## Install (via tcadm; future)

```bash
tcadm agent install tritonagent
tcadm agent configure tritonagent --endpoint http://172.16.96.4:8080
# (tcadm agent install also accepts --endpoint to skip the configure
# step; pending implementation under P7-ish.)
```

## Build flow

```bash
# 1. Build tritonagent ALONE — see warning below.
cargo build --release -p tritonagent

# 2. Push the binary to ~~/public/tritoncloud/sources/
mput -f target/release/tritonagent ~~/public/tritoncloud/sources/tritonagent-illumos.bin

# 3. Package + publish the tarball.
STAMP=$(date -u +%Y%m%dT%H%M%SZ) bash agents/tritonagent/build.sh
tritoncloud-publish --channel edge agent \
    --name tritonagent \
    --stamp "$STAMP" \
    --tarball /tmp/tritonagent-$STAMP.tar.gz
```

The build script fetches the binary from
`~~/public/tritoncloud/sources/tritonagent-illumos.bin` on demand if
`proto/opt/triton/tritonagent/bin/tritonagent` is missing.

### IMPORTANT: build tritonagent in its OWN cargo invocation

Do NOT combine `cargo build -p tritond --features foundationdb`
with `-p tritonagent` in the same invocation. Cargo's feature
unification leaks the `foundationdb` feature from tritond down
into the shared `tritond-store` rlib; tritonagent then links
against `libfdb_c.so` unnecessarily and refuses to start in any
GZ that doesn't have it on `LD_LIBRARY_PATH`.

Run the two builds as separate cargo commands:

```bash
cargo build --release -p tritond --features foundationdb
cargo build --release -p tritonagent
```

A clean tritonagent binary (`ldd $bin | grep fdb` returns nothing)
is the only thing the publisher should upload.

## On first registration

tritonagent self-registers with tritond on first boot:

1. It POSTs `/v2/cn/register` with this CN's UUID + admin IP.
2. tritond returns a claim code; the agent prints it to its log
   (`/var/log/tritonagent/agent.out`).
3. An operator runs `tcadm cn approve <claim-code>` from a
   workstation talking to tritond.
4. tritond hands back a per-CN API key; tritonagent persists it at
   `/var/lib/tritonagent/credentials` and proceeds.

Subsequent boots skip step 1; the credential file is the resume marker.
