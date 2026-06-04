<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# `mantas3-prober`

Black-box synthetic-request prober for the mantas3 S3 surface. Slice 1
of `monitor-reef-zy0v`.

The prober issues PUT/GET/HEAD/DELETE cycles against a configured
mantad endpoint on a tight interval and exposes Prometheus metrics on
`/metrics`. Its purpose is to catch wedge-style failures (where
internal `/metrics` counters keep ticking but real request handlers
are stuck — see `monitor-reef-69bg`) within one probe cycle instead of
"whenever someone next runs a test."

## Required env vars (4)

| Var | Example | Notes |
|---|---|---|
| `MANTAS3_PROBER_ENDPOINT` | `http://192.168.1.182:7443` or `https://s3.example.com` | full URL including scheme |
| `MANTAS3_PROBER_REGION` | `us-east-1` | SigV4 signing region (technically optional with default `us-east-1`, but always set in production) |
| `MANTAS3_PROBER_BUCKET` | `prober-canary` | pre-created bucket; operator owns its lifecycle |
| `MANTAS3_PROBER_ACCESS_KEY_ID` | `AKIA...` | SigV4 access key id |
| `MANTAS3_PROBER_SECRET_ACCESS_KEY` | `...` | SigV4 secret — never logged; the `Debug` impl is redacted |

## Tuning env vars (7)

| Var | Default | Notes |
|---|---|---|
| `MANTAS3_PROBER_INTERVAL_SECS` | `30` | period between cycle starts |
| `MANTAS3_PROBER_OP_TIMEOUT_SECS` | `10` | per-op timeout; must be strictly less than `INTERVAL_SECS` |
| `MANTAS3_PROBER_PAYLOAD_BYTES` | `4096` | PUT payload size; 4 KiB matches the SLO's small-object class |
| `MANTAS3_PROBER_METRICS_PORT` | `9275` | Prometheus scrape port |
| `MANTAS3_PROBER_METRICS_BIND` | `0.0.0.0` | bind address for the `/metrics` listener |
| `MANTAS3_PROBER_LOG_LEVEL` | `info` | `tracing_subscriber` env-filter expression |
| `MANTAS3_PROBER_AUTH_WARN_THRESHOLD` | `3` | consecutive auth-failed cycles before the I6 WARN log fires |

## Metrics

Series exposed on `/metrics`:

| Series | Type | Notes |
|---|---|---|
| `mantas3_prober_op_duration_seconds{op,outcome}` | histogram | `op` ∈ {put, get, head, delete}; `outcome` ∈ {success, timeout, error_4xx, error_5xx, sdk_error}. `_count` and `_sum` are auto-emitted. |
| `mantas3_prober_op_errors_total{op,code}` | counter | per-op error distribution by HTTP status (`403`, `404`, `500`, ...) or class (`timeout`, `sdk_no_response`). |
| `mantas3_prober_data_integrity_failures_total` | counter | PUT-then-GET body mismatch. **Data corruption.** Higher severity than a wedge. |
| `mantas3_prober_auth_failures_total` | counter | 403 on any op. Distinct from wedge so creds misconfiguration isn't confused with target failure. |
| `mantas3_prober_cycle_panics_total` | counter | the prober itself panicked. SEV-1: monitoring is broken. Distinct from `cycle_success`. |
| `mantas3_prober_cycle_success` | gauge | 1 if every op in the most recent cycle succeeded, 0 otherwise. The alerting signal. |
| `mantas3_prober_cycle_interval_seconds` | gauge | configured cycle interval. Alert rules can self-describe (`for: 3 * mantas3_prober_cycle_interval_seconds`). |

## Log schema

Each cycle emits N+1 structured JSON lines on stdout:

```jsonc
// per-op
{
  "event": "probe_op",
  "cycle_id": "<uuidv4>",
  "op": "put|get|head|delete",
  "outcome": "success|timeout|error_4xx|error_5xx|sdk_error",
  "http_status": "200|403|...",  // null if no HTTP response
  "key": "probe-<uuidv4>",
  "sdk_error_chain": [
    {"error_type": "...", "message": "..."},
    {"error_type": "...", "message": "..."}
  ]
}

// per-cycle summary
{
  "event": "probe_cycle",
  "cycle_id": "<uuidv4>",
  "outcome": "success|failed",
  "had_auth_failure": false,
  "had_data_integrity_failure": false,
  "total_latency_ms": 45.2
}
```

The 3am-page debug path is: page fires → operator searches by
`cycle_id` → all N+1 lines for that cycle land together with the
SDK error chain inline.

## Operator maintenance: stale-key cleanup

The prober makes a best-effort DELETE in step 4 of each cycle, but does
not guarantee bucket hygiene. If DELETEs fail systematically, probe
keys accumulate. **This is operator business** — a self-cleaning prober
would create a new failure mode (two probers deleting each other's
keys-under-test).

Recommended sweep (cron-driven, weekly):

```bash
aws s3api list-objects-v2 \
  --bucket "$MANTAS3_PROBER_BUCKET" \
  --prefix probe- \
  --query "Contents[?LastModified<\`$(date -u -d '7 days ago' +%FT%TZ)\`].Key" \
  --output text \
  | xargs -n 100 aws s3api delete-objects --bucket "$MANTAS3_PROBER_BUCKET" --delete
```

Adjust the cutoff to taste; anything older than `INTERVAL_SECS * 2` is
guaranteed leaked.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | clean shutdown (SIGTERM / SIGINT) |
| 2 | `MANTAS3_PROBER_*` env var missing or invalid |
| 3 | Prometheus registry initialization failed (programming error) |
| 4 | startup `HeadBucket` returned an error (bucket missing, creds wrong, endpoint unreachable) |
| 5 | startup `HeadBucket` timed out |
| 6 | `/metrics` listener task terminated unexpectedly (R2 supervision tripped) |

Any non-zero exit means "fix the deployment and let SMF restart with
backoff." The prober deliberately does NOT exit on observed target
failures — those are emitted as metrics, not exit codes.

## Tier-3 live smoke procedure

Documented for the operator's post-deploy verification. Not part of
`cargo test` — requires a live cluster.

1. Set env vars at the test rig:
   ```sh
   export MANTAS3_PROBER_ENDPOINT=http://192.168.1.182:7443
   export MANTAS3_PROBER_REGION=us-east-1
   export MANTAS3_PROBER_BUCKET=prober-canary
   export MANTAS3_PROBER_ACCESS_KEY_ID=<...>
   export MANTAS3_PROBER_SECRET_ACCESS_KEY=<...>
   ```
2. Pre-create the bucket as the prober's owner.
3. Run `mantas3-prober &` for 5 minutes.
4. `curl http://localhost:9275/metrics | grep mantas3_prober` and
   verify `mantas3_prober_op_duration_seconds_count{outcome="success"}` is
   non-zero for every `op` label.
5. Kill mantad's S3 listener; verify `mantas3_prober_cycle_success`
   drops to 0 within one interval.
6. Restart mantad; verify recovery within one interval.
7. Unset `MANTAS3_PROBER_BUCKET` to point at a non-existent bucket and
   restart; verify the daemon exits non-zero (I1).

## Deployment

Single binary. Recommended host: the operator zone, alongside other
ops tooling. SMF or systemd unit ships in a follow-up bead once the
deploy story (credential provisioning, which user owns the prober
bucket, where the bucket lives) is settled.

## References

- Design: `~/.claude/plans/zy0v-slice1-prober.md` (nine-reviewers Approve round 2)
- Parent: `monitor-reef-zy0v` (umbrella)
- Slice 1: `monitor-reef-0sz2` (this crate)
- Slice 2: `monitor-reef-aw47` (SLI/SLO doc)
- Slice 3: `monitor-reef-itg4` (Grafana dashboards)
- Slice 4: `monitor-reef-cpjr` (Prometheus alert rules)
- Canonical wedge: `monitor-reef-69bg`
