# M1 Lab Smoke With Current Tools

This is the first repeatable M1 integration harness. It intentionally
uses today's `tcadm` and API surfaces instead of waiting for the
future end-user `triton-vnext` CLI. The goal is to expose the first
real SmartOS hardware blocker in the project/VPC/subnet/NAT/route/
instance/FIP path.

Script:

```bash
scripts/m1-smoke.sh --execute
```

Default mode is `--dry-run`, which writes the command plan to a
timestamped log directory without mutating the lab.

## Inputs

The current operator CLI requires UUIDs for tenant-scoped resources,
so the script cannot yet say "tenant default" the way the final
end-user smoke will. Set the default tenant UUID explicitly:

```bash
export TCADM_ENDPOINT=http://10.199.199.10:8080
export TCADM_API_KEY=...
export M1_TENANT_ID=<default-tenant-uuid>
```

If the bootstrap root password was not saved from the first `tritond`
startup banner, reset it locally on the headnode before running
`tcadm configure`:

```bash
tritond reset-root-password --fdb-cluster-file /etc/fdb.cluster
```

The reset command prints a new root password once. It updates only the
stored password hash; it does not wipe tenant, project, CN, image, or
network records.

The lab CN defaults match the current M1 split:

| Role | Default host | Default server UUID |
| --- | --- | --- |
| tenant CN | `10.199.199.41` (`nuc0`) | `f7d2efb6-8c3b-e1fe-111f-88aedd065474` |
| edge CN | `10.199.199.40` (`nuc1`) | `8b2a9975-6354-8a94-39e4-1c697aa96b33` |

The script sets those roles with:

```bash
tcadm cn label set "$M1_TENANT_CN_UUID" --role tenant
tcadm cn label set "$M1_EDGE_CN_UUID" --role edge
```

For guest provisioning, either provide a concrete image and SSH key:

```bash
export M1_IMAGE_ID=<bhyve-linux-image-uuid>
export M1_SSH_KEY_ID=<ssh-key-uuid>
```

or let the script create a project-scoped SSH key from
`M1_SSH_PUBLIC_KEY_FILE` and select the first project-visible image
with `os` that looks Linux-like and `compatibility.brand == "bhyve"`.

## What It Checks

The script creates or reuses, by name:

- project `sandbox`
- VPC `prod` with `10.0.0.0/16`
- subnet `app` with `10.0.1.0/24`
- NAT gateway `egress`
- route `0.0.0.0/0 -> nat-gateway:<id>` in the VPC main route table
- instance `web`
- floating IP `web-fip`, attached to the instance primary NIC

It then polls:

- instance lifecycle until `running`
- NAT `realized.applied_generation >= desired_generation` and
  `edge_cluster_id` is present

By default it also probes the real dataplane:

- `proteusadm dump ports` and `proteusadm dump rules` on the tenant CN
- `pgrep -fl fhrun` on the edge CN
- guest SSH to the floating IP
- guest egress with `curl -fsS --max-time 30 https://1.1.1.1`, falling
  back to `https://9.9.9.9`

Each command, assertion, and JSON response is written under
`/tmp/triton-vnext-m1-<timestamp>/`. Stable IDs are also written to
`ids.env` in that directory.

## Current Limitations

This is not the final S15 acceptance script yet.

- It uses `tcadm`, not the future `triton-vnext` user CLI.
- It requires `M1_TENANT_ID` because `tcadm` cannot resolve the default
  tenant by name.
- It polls NAT realization through `tcadm net nat-gw get`; there is no
  `tcadm edge` read surface yet.
- It does not call `tcadm doctor` because that subcommand is still a
  separate MVP gap.
- Cleanup is opt-in with `--cleanup` so failed lab runs preserve state
  for debugging.

Once the first lab run passes with this harness, promote the same flow
into the final workspace-level `triton-vnext/scripts/m1-smoke.sh` using
the user-facing CLI and add `tcadm doctor` assertions.
