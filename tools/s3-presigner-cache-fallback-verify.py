#!/usr/bin/env python3
"""Phase 2 followups — presigner cache eviction + cluster-root fallback.

Closes the two followups documented in
`evidence/s3-presigner-tritond-verify-20260531.md`:

  (3) Cache eviction on `drop_silo_tenant_storage` — unit-tested
      (`presigner_cache::evict_is_idempotent`) but never exercised
      end-to-end. This script binds a tenant, mints a presign
      (warms the cache), drops the storage binding, re-inits, and
      mints again. The post-rebind URL's `X-Amz-Credential` AKID
      must differ from the pre-drop AKID — proves the cache
      released the stale entry and the next sign went through a
      fresh fetch.

      NOTE: running this surfaced bd monitor-reef-n4w7 — mantad's
      `delete_workspace` refuses a non-empty workspace and the
      Phase 2 presigner-system user counts as "not empty," so
      `drop_silo_tenant_storage` 409s before it can call
      `presigner_cache.evict`. The verify works around this by
      DELETE-ing `presigner-{workspace}` via the fleet-admin
      forwarder before `drop-storage`. That keeps the eviction
      code path live (the tritond handler still runs the evict
      call after the now-successful delete_workspace) while we
      track the cascade fix in n4w7.

  (4) Cluster-root presigner fallback — Phase 2 verified the
      per-workspace path; the Unscoped / fleet-admin fallback
      path is configured but no direct test that it *fires*
      for an unscoped caller. This script mints a presign as
      a fleet-admin (Unscoped scope) and confirms the URL's
      AKID matches `cluster.presigner_access_key_id` (the
      AKIA97EF... cluster-root key, NOT a per-workspace
      MKIASA... key).

Runs from anywhere with SSH access to 192.168.1.182. Test-box
auth, tcadm, and admin curl all stay server-side.

Exit code is the number of failed assertions.
"""

import argparse
import json
import re
import secrets
import subprocess
import sys
from urllib.parse import parse_qs, urlparse


def run_ssh(host: str, cmd: str, *, user: str = "root", check: bool = True) -> tuple[int, str, str]:
    p = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", f"{user}@{host}", cmd],
        capture_output=True,
        text=True,
    )
    if check and p.returncode != 0:
        raise RuntimeError(
            f"ssh {user}@{host} {cmd!r} failed (rc={p.returncode}): {p.stderr}"
        )
    return p.returncode, p.stdout, p.stderr


def ssh_out(host: str, cmd: str) -> str:
    _, out, _ = run_ssh(host, cmd)
    return out


def tcadm(host: str, *args: str) -> str:
    return ssh_out(host, "/opt/triton/bin/tcadm " + " ".join(args))


def tritond_request(
    host: str,
    method: str,
    path: str,
    body: dict | None,
    token: str | None,
) -> tuple[int, str]:
    """Generic HTTP to tritond from inside the test box."""
    args = [
        f"curl -sS -o /dev/stdout -w '\\n%{{http_code}}' -X {method}",
    ]
    if body is not None or method in ("POST", "PUT"):
        args.append("-H 'Content-Type: application/json'")
        body_str = json.dumps(body or {})
        args.append(f"-d {json.dumps(body_str)}")
    if token:
        args.append(f"-H 'Authorization: Bearer {token}'")
    args.append(f"http://{host}:8080{path}")
    # Don't strip — empty-body responses look like `\n204`, and a
    # leading `.strip()` collapses them to just `204` (no body
    # separator), defeating the rsplit. Strip the trailing newline
    # only.
    out = ssh_out(host, " ".join(args)).rstrip("\n")
    lines = out.rsplit("\n", 1)
    if len(lines) == 2:
        return int(lines[1]), lines[0]
    # Empty body: the whole output is the status line.
    if out.isdigit():
        return int(out), ""
    return 0, out


def tritond_post(host: str, path: str, body: dict | None, token: str | None = None) -> tuple[int, str]:
    return tritond_request(host, "POST", path, body, token)


def tritond_delete(host: str, path: str, token: str | None) -> tuple[int, str]:
    return tritond_request(host, "DELETE", path, None, token)


def login(host: str, username: str, password: str) -> str:
    status, body = tritond_post(
        host, "/v1/auth/login", {"username": username, "password": password}
    )
    if status != 200:
        raise RuntimeError(f"login {username} -> {status}: {body}")
    return json.loads(body)["access_token"]


def fleet_admin_token(host: str) -> str:
    """Read the cached tcadm fleet-admin token off the test box.
    Uses ` *` (space-asterisk) instead of `\\s*` so BSD sed on
    illumos parses the regex (illumos sed doesn't grok `\\s`)."""
    out = ssh_out(
        host,
        "sed -n 's/.*\"access_token\": *\"\\([^\"]*\\)\".*/\\1/p' "
        "/root/.config/tcadm/config.json | head -1",
    ).strip()
    if not out:
        raise RuntimeError(
            "no cached fleet-admin token in /root/.config/tcadm/config.json — "
            "run `tcadm login` on the test box first"
        )
    return out


def extract_akid(url: str) -> str | None:
    qs = parse_qs(urlparse(url).query)
    cred = qs.get("X-Amz-Credential", [None])[0]
    if not cred:
        return None
    return cred.split("/", 1)[0]


def presign_put_via_tritond(host: str, token: str, cluster_id: str, bucket: str, key: str) -> tuple[int, str]:
    return tritond_post(
        host,
        f"/v1/storage/clusters/{cluster_id}/s3/presign/put",
        {"bucket": bucket, "key": key, "expires_secs": 300},
        token=token,
    )


def workspace_name_for(tenant_id: str) -> str:
    return "t-" + tenant_id.replace("-", "")


def delete_presigner_user(
    host: str, admin_token: str, cluster_id: str, workspace: str
) -> tuple[int, str]:
    """Work around bd monitor-reef-n4w7: explicitly delete the
    presigner-{workspace} IAM user so the workspace becomes
    eligible for delete_workspace. Idempotent: 404 is fine."""
    return tritond_delete(
        host,
        f"/v1/storage/clusters/{cluster_id}/users/presigner-{workspace}",
        admin_token,
    )


def discover_cluster_id(host: str) -> str:
    raw = ssh_out(
        host,
        "/opt/triton/bin/tcadm storage cluster list --json 2>/dev/null"
        " || /opt/triton/bin/tcadm storage cluster list",
    )
    m = re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", raw)
    if not m:
        raise RuntimeError("no cluster registered on tritond")
    return m.group(0)


def cluster_root_akid(host: str, cluster_id: str) -> str | None:
    out = ssh_out(
        host, f"/opt/triton/bin/tcadm storage cluster show {cluster_id} --json"
    )
    try:
        return json.loads(out).get("presigner_access_key_id")
    except json.JSONDecodeError:
        return None


def cleanup_orphan_tenant(
    host: str, admin_token: str, cluster_id: str, silo_id: str, tenant_id: str
) -> None:
    """Best-effort cleanup of a tenant left behind by a previous
    failed run. Deletes the presigner-system user, drops the
    storage binding, and deletes the tenant row. Silent on
    'tenant already gone' / 'no binding' errors."""
    ws = workspace_name_for(tenant_id)
    s, body = delete_presigner_user(host, admin_token, cluster_id, ws)
    if s not in (200, 204, 404):
        print(f"  WARN cleanup: delete presigner-{ws[:24]}... -> {s}: {body[:120]}")
    rc, _, err = run_ssh(
        host, f"/opt/triton/bin/tcadm tenant drop-storage {silo_id} {tenant_id}", check=False
    )
    if rc != 0 and "tenant has no storage binding" not in err and "not found" not in err:
        print(f"  WARN cleanup: drop-storage {tenant_id[:8]} -> {err.strip()[:160]}")
    rc, _, err = run_ssh(
        host, f"/opt/triton/bin/tcadm tenant delete {silo_id} {tenant_id}", check=False
    )
    if rc != 0 and "not found" not in err:
        print(f"  WARN cleanup: tenant delete {tenant_id[:8]} -> {err.strip()[:160]}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mantad-host", default="192.168.1.182")
    ap.add_argument(
        "--silo-id",
        default="988ae554-3508-44cc-afb8-a2300fbeaf13",
        help="Silo to provision the verify tenant into (default: phase-c silo).",
    )
    ap.add_argument(
        "--cleanup-tenant",
        help="Cleanup an orphan tenant id from a prior failed run, then exit.",
    )
    args = ap.parse_args()
    fails = 0
    host = args.mantad_host
    silo_id = args.silo_id

    print("== Discovery ==")
    cluster_id = discover_cluster_id(host)
    root_akid = cluster_root_akid(host, cluster_id)
    print(f"  cluster_id:    {cluster_id}")
    print(f"  root_akid:     {root_akid}")
    if not root_akid:
        print("  FAIL cluster has no presigner_access_key_id configured "
              "(run tools/setup-presigner.sh first)")
        return 1

    admin_token = fleet_admin_token(host)
    print(f"  fleet-admin token len: {len(admin_token)}")

    if args.cleanup_tenant:
        print(f"\n== Cleanup orphan {args.cleanup_tenant} ==")
        cleanup_orphan_tenant(host, admin_token, cluster_id, silo_id, args.cleanup_tenant)
        return 0

    # -----------------------------------------------------------------
    # Item 3: cache eviction on drop_silo_tenant_storage.
    # -----------------------------------------------------------------
    print("\n== Item 3: cache eviction on drop_silo_tenant_storage ==")

    suffix = secrets.token_hex(2)
    tenant_name = f"cache-evict-{suffix}"
    out = tcadm(host, "tenant", "create", "--name", tenant_name, silo_id)
    tenant_id = re.search(r"[0-9a-f-]{36}", out).group(0)
    workspace = workspace_name_for(tenant_id)
    print(f"  tenant created: {tenant_id} ({tenant_name})")
    print(f"  workspace:      {workspace}")

    uname = f"alice-cache-{suffix}"
    pw = secrets.token_hex(16)
    tcadm(
        host, "tenant", "create-user",
        "--username", uname, "--password", pw,
        silo_id, tenant_id,
    )
    token = login(host, uname, pw)
    print(f"  operator logged in: {uname}")

    # Pre-drop mint: warms the (cluster_id, workspace) cache entry.
    s, resp = presign_put_via_tritond(
        host, token, cluster_id, "cache-evict-probe", "probe-pre.txt"
    )
    if s != 200:
        print(f"  FAIL pre-drop presign -> {s}: {resp[:200]}")
        cleanup_orphan_tenant(host, admin_token, cluster_id, silo_id, tenant_id)
        return fails + 1
    pre_akid = extract_akid(json.loads(resp)["url"])
    print(f"  pre-drop  AKID: {pre_akid}")
    if pre_akid == root_akid:
        print("  FAIL pre-drop URL signed with cluster-root AKID — "
              "tenant scope didn't engage")
        fails += 1

    # Workaround for bd monitor-reef-n4w7: explicitly delete the
    # presigner-system user so the workspace is empty by mantad's
    # standards. Once n4w7 is fixed (cascade on the mantad side, or
    # explicit delete-user on the tritond side), this DELETE call
    # can be removed and drop-storage alone will exercise the
    # cache eviction path.
    s, body = delete_presigner_user(host, admin_token, cluster_id, workspace)
    if s not in (200, 204):
        print(f"  FAIL pre-drop delete presigner-{workspace[:16]}... -> {s}: {body[:200]}")
        cleanup_orphan_tenant(host, admin_token, cluster_id, silo_id, tenant_id)
        return fails + 1
    print(f"  pre-drop workaround: deleted presigner-{workspace[:16]}... (n4w7 workaround)")

    # Drop the storage binding. With the presigner gone, mantad's
    # delete_workspace succeeds; tritond's handler then calls
    # `presigner_cache.evict(cluster_id, workspace)`. Cache state
    # is what the rest of this test is checking.
    out = tcadm(host, "tenant", "drop-storage", silo_id, tenant_id)
    print(f"  drop-storage:   {out.strip()[:120] or '(silent)'}")

    # Re-init: new mantad workspace (same name, since name is derived
    # from tenant_id) and a new presigner-system user with a fresh
    # access key id.
    out = tcadm(host, "tenant", "init-storage", silo_id, tenant_id)
    print(f"  init-storage:   {out.strip()[:120] or '(silent)'}")

    # Post-rebind mint: cache miss (because eviction ran) -> fresh
    # fetch -> AKID differs from pre_akid. If eviction was broken,
    # cache would still hold pre_akid and the assertion would fail.
    s, resp = presign_put_via_tritond(
        host, token, cluster_id, "cache-evict-probe", "probe-post.txt"
    )
    if s != 200:
        print(f"  FAIL post-rebind presign -> {s}: {resp[:200]}")
        cleanup_orphan_tenant(host, admin_token, cluster_id, silo_id, tenant_id)
        return fails + 1
    post_akid = extract_akid(json.loads(resp)["url"])
    print(f"  post-rebind AKID: {post_akid}")

    if pre_akid == post_akid:
        print("  FAIL pre-drop AKID == post-rebind AKID — cache did "
              "NOT evict on drop_silo_tenant_storage (stale cache hit)")
        fails += 1
    else:
        print("  OK   pre-drop AKID != post-rebind AKID — "
              "cache evicted, fresh fetch on rebind")

    if post_akid == root_akid:
        print("  FAIL post-rebind URL signed with cluster-root AKID — "
              "per-workspace path not taken on re-init")
        fails += 1

    # -----------------------------------------------------------------
    # Item 4: cluster-root fallback for an Unscoped caller.
    # -----------------------------------------------------------------
    print("\n== Item 4: cluster-root presigner fallback (Unscoped) ==")

    # Fleet-admin (root) -> WorkspaceListAcrossTenants -> Unscoped
    # scope -> workspace_name() = None -> mint_presigned_url falls
    # through to cluster.presigner_access_key_id (the cluster-root
    # AKIA97EF... key, NOT a per-workspace MKIASA... key).
    s, resp = presign_put_via_tritond(
        host, admin_token, cluster_id, "fallback-probe", "probe.txt"
    )
    if s != 200:
        print(f"  FAIL admin presign -> {s}: {resp[:200]}")
        fails += 1
    else:
        fb_akid = extract_akid(json.loads(resp)["url"])
        print(f"  admin URL AKID: {fb_akid}")
        if fb_akid == root_akid:
            print("  OK   admin URL signed with cluster-root AKID — "
                  "Unscoped fallback path engaged")
        else:
            print(f"  FAIL admin URL AKID != cluster-root "
                  f"({fb_akid} vs {root_akid}) — fallback path did "
                  "NOT engage for Unscoped caller")
            fails += 1

    # -----------------------------------------------------------------
    # Cleanup
    # -----------------------------------------------------------------
    print("\n== Cleanup ==")
    cleanup_orphan_tenant(host, admin_token, cluster_id, silo_id, tenant_id)
    print(f"  cleaned up {tenant_id[:8]}")

    print(f"\n== Result: {fails} failure(s) ==")
    return fails


if __name__ == "__main__":
    sys.exit(main())
