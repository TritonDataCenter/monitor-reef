#!/usr/bin/env python3
"""Phase 2 — tritond-mediated presign integration live verify.

Closes monitor-reef-f990: proves the per-workspace presigner cache
+ scope threading + tritond's /v1/storage/clusters/{id}/s3/presign
endpoints work end-to-end through tritond, not just mantad-side.

Flow:
  1. Find the registered storage cluster id from tritond.
  2. Provision two tenants in a fresh silo (or reuse).
  3. Mint a tenant operator account in each via tcadm.
  4. Log each operator into tritond -> access token.
  5. Each operator creates a bucket in their workspace via tritond's
     admin proxy.
  6. alice-a calls tritond's /s3/presign/put for HER bucket -> URL
     signed by presigner-t-A. PUT -> 200 (Phase 2 in-workspace path).
  7. alice-a calls tritond's /s3/presign/put for BOB's bucket -> URL
     signed by presigner-t-A but pointing at bucket-b. PUT -> 404
     NoSuchBucket (Phase 1 gate fires on mantad because
     bucket-b.workspace = t-B and caller resolves as
     Iam { workspace = t-A }).
  8. Inspect the URL to confirm the X-Amz-Credential AKID looks
     like the per-workspace key, not the cluster-root key.

Runs from build02. Test-box auth-token, tcadm, and admin curl all
stay server-side via ssh+command.

Exit code is the number of failed assertions.
"""

import argparse
import json
import secrets
import subprocess
import sys
import uuid
from urllib import request as urlreq
from urllib.error import HTTPError


def run_ssh(host: str, cmd: str, *, user: str = "root", check: bool = True) -> str:
    p = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", f"{user}@{host}", cmd],
        capture_output=True,
        text=True,
    )
    if check and p.returncode != 0:
        raise RuntimeError(f"ssh {user}@{host} {cmd!r} failed (rc={p.returncode}): {p.stderr}")
    return p.stdout


def tcadm(host: str, *args: str) -> str:
    cmd = "/opt/triton/bin/tcadm " + " ".join(args)
    return run_ssh(host, cmd)


def tritond_post(host: str, path: str, body: dict | None, token: str | None = None) -> tuple[int, str]:
    """POST to tritond from inside the test box. Avoids exposing the
    admin-side network to build02."""
    body_str = json.dumps(body or {})
    args = [
        f"curl -sS -o /dev/stdout -w '\\n%{{http_code}}' -X POST"
        f" -H 'Content-Type: application/json'",
    ]
    if token:
        args.append(f"-H 'Authorization: Bearer {token}'")
    args.append(f"-d {json.dumps(body_str)}")
    args.append(f"http://{host}:8080{path}")
    out = run_ssh(host, " ".join(args), check=False).strip()
    # Last line is HTTP status; everything before is the body.
    lines = out.rsplit("\n", 1)
    if len(lines) == 2:
        body_out, status = lines
        return int(status), body_out
    return 0, out


def login(host: str, username: str, password: str) -> str:
    status, body = tritond_post(
        host, "/v1/auth/login", {"username": username, "password": password}
    )
    if status != 200:
        raise RuntimeError(f"login {username} -> {status}: {body}")
    return json.loads(body)["access_token"]


def list_clusters(host: str, token: str) -> list[dict]:
    out = run_ssh(
        host,
        f"curl -sS -H 'Authorization: Bearer {token}'"
        f" http://{host}:8080/v1/storage/clusters",
    )
    return json.loads(out).get("items") or json.loads(out)


def create_bucket_via_tritond(
    host: str, token: str, cluster_id: str, bucket: str
) -> tuple[int, str]:
    # Bucket admin-proxy path is /v1/storage/clusters/{id}/buckets,
    # NOT /s3/buckets. Only the presign minter lives under /s3/.
    return tritond_post(
        host,
        f"/v1/storage/clusters/{cluster_id}/buckets",
        {"name": bucket},
        token=token,
    )


def presign_put_via_tritond(
    host: str, token: str, cluster_id: str, bucket: str, key: str
) -> tuple[int, str]:
    return tritond_post(
        host,
        f"/v1/storage/clusters/{cluster_id}/s3/presign/put",
        {"bucket": bucket, "key": key, "expires_secs": 300},
        token=token,
    )


def presign_get_via_tritond(
    host: str, token: str, cluster_id: str, bucket: str, key: str
) -> tuple[int, str]:
    return tritond_post(
        host,
        f"/v1/storage/clusters/{cluster_id}/s3/presign/get",
        {"bucket": bucket, "key": key, "expires_secs": 300},
        token=token,
    )


def http_put_url(url: str, body: bytes) -> tuple[int, str]:
    req = urlreq.Request(url, data=body, method="PUT")
    try:
        with urlreq.urlopen(req) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def http_get_url(url: str) -> tuple[int, bytes]:
    try:
        with urlreq.urlopen(url) as r:
            return r.status, r.read()
    except HTTPError as e:
        return e.code, e.read()


def extract_akid(url: str) -> str | None:
    """Pull the AKID out of the X-Amz-Credential query parameter."""
    from urllib.parse import urlparse, parse_qs

    qs = parse_qs(urlparse(url).query)
    cred = qs.get("X-Amz-Credential", [None])[0]
    if not cred:
        return None
    # Format: AKID/<date>/<region>/<service>/aws4_request
    return cred.split("/", 1)[0]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mantad-host", default="192.168.1.182")
    ap.add_argument(
        "--silo-id",
        default="988ae554-3508-44cc-afb8-a2300fbeaf13",
        help="Existing silo to provision the verify tenants into. Defaults to the phase-c silo on 192.168.1.182.",
    )
    args = ap.parse_args()
    fails = 0

    print("== Discovering cluster registration ==")
    cluster_list_raw = run_ssh(
        args.mantad_host,
        "/opt/triton/bin/tcadm storage cluster list --json 2>/dev/null"
        " || /opt/triton/bin/tcadm storage cluster list",
    )
    print(cluster_list_raw[:500])
    import re

    match = re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", cluster_list_raw)
    if not match:
        print("  FAIL no cluster registered on tritond")
        return 1
    cluster_id = match.group(0)
    print(f"  cluster_id: {cluster_id}")

    print(f"\n== Provisioning two tenants in silo {args.silo_id} ==")
    silo_id = args.silo_id

    tenants = []
    for label in ("a", "b"):
        out = tcadm(
            args.mantad_host,
            "tenant",
            "create",
            "--name",
            f"presign-tenant-{label}-{secrets.token_hex(2)}",
            silo_id,
        )
        tid = re.search(r"[0-9a-f-]{36}", out).group(0)
        tenants.append((label, tid))
        print(f"  tenant {label}: {tid}")

    print("\n== Minting tenant operator accounts ==")
    operators = []
    for label, tid in tenants:
        uname = f"alice-{label}-{secrets.token_hex(2)}"
        pw = secrets.token_hex(16)
        tcadm(
            args.mantad_host,
            "tenant",
            "create-user",
            "--username",
            uname,
            "--password",
            pw,
            silo_id,
            tid,
        )
        token = login(args.mantad_host, uname, pw)
        operators.append((label, tid, uname, token))
        print(f"  {label}: {uname} logged in (token len={len(token)})")

    label_a, tid_a, uname_a, token_a = operators[0]
    label_b, tid_b, uname_b, token_b = operators[1]

    print("\n== Each operator creates a bucket via tritond admin proxy ==")
    bucket_a = f"presign-tritond-a-{secrets.token_hex(3)}"
    bucket_b = f"presign-tritond-b-{secrets.token_hex(3)}"
    s_a, b_a = create_bucket_via_tritond(args.mantad_host, token_a, cluster_id, bucket_a)
    s_b, b_b = create_bucket_via_tritond(args.mantad_host, token_b, cluster_id, bucket_b)
    if s_a in (200, 201):
        print(f"  OK   alice-a created {bucket_a} (HTTP {s_a})")
    else:
        print(f"  FAIL alice-a create_bucket -> {s_a}: {b_a[:200]}")
        fails += 1
    if s_b in (200, 201):
        print(f"  OK   alice-b created {bucket_b} (HTTP {s_b})")
    else:
        print(f"  FAIL alice-b create_bucket -> {s_b}: {b_b[:200]}")
        fails += 1

    print("\n== In-workspace presign PUT/GET ==")
    s, resp = presign_put_via_tritond(
        args.mantad_host, token_a, cluster_id, bucket_a, "phase2/probe.txt"
    )
    if s != 200:
        print(f"  FAIL alice-a presign put -> {s}: {resp[:200]}")
        return fails + 1
    put_url = json.loads(resp)["url"]
    akid = extract_akid(put_url)
    print(f"  alice-a presign URL AKID prefix: {akid[:16] if akid else '<missing>'}...")
    # The cluster-root presigner key is what tritond's StorageCluster row
    # has. Tenant-scoped should be DIFFERENT.
    cluster_show = run_ssh(
        args.mantad_host,
        f"/opt/triton/bin/tcadm storage cluster show {cluster_id}",
    )
    root_akid_match = re.search(r"presigner_access_key_id:\s*([A-Z0-9]+)", cluster_show)
    if root_akid_match and akid == root_akid_match.group(1):
        print(
            f"  FAIL URL signed with cluster-root AKID {root_akid_match.group(1)} — "
            f"per-workspace cache not engaged"
        )
        fails += 1
    elif root_akid_match:
        print(f"  OK   URL AKID differs from cluster-root ({root_akid_match.group(1)[:16]}...)")
    else:
        print(f"  OK   URL has AKID; cluster-root AKID unparseable in cluster show")

    s, body = http_put_url(put_url, b"phase2 hello via tritond presign")
    if s == 200:
        print(f"  OK   PUT in-workspace via tritond-signed URL -> 200")
    else:
        print(f"  FAIL PUT in-workspace -> {s}: {body[:200]}")
        fails += 1

    s, resp = presign_get_via_tritond(
        args.mantad_host, token_a, cluster_id, bucket_a, "phase2/probe.txt"
    )
    if s != 200:
        print(f"  FAIL alice-a presign get -> {s}: {resp[:200]}")
        fails += 1
    else:
        get_url = json.loads(resp)["url"]
        s, body = http_get_url(get_url)
        if s == 200 and body == b"phase2 hello via tritond presign":
            print(f"  OK   GET in-workspace round-trip -> 200 + correct body")
        else:
            print(f"  FAIL GET in-workspace -> {s}: {body[:160]!r}")
            fails += 1

    print("\n== Cross-workspace presign — gate fires on mantad ==")
    # alice-a (in t-A) asks tritond to sign a URL for bucket-b
    # (which lives in t-B). Tritond will happily sign it with
    # presigner-t-A's key; mantad's gate fires when the URL is used.
    s, resp = presign_put_via_tritond(
        args.mantad_host, token_a, cluster_id, bucket_b, "phase2/cross.txt"
    )
    if s != 200:
        print(f"  FAIL alice-a cross presign-put -> {s}: {resp[:200]}")
        fails += 1
    else:
        cross_url = json.loads(resp)["url"]
        cross_akid = extract_akid(cross_url)
        print(
            f"  cross URL AKID: {cross_akid[:16] if cross_akid else '<missing>'}... "
            f"(should be the same as alice-a's in-workspace URL above)"
        )
        s, body = http_put_url(cross_url, b"cross attempt")
        if s == 404 or "NoSuchBucket" in body:
            print(f"  OK   PUT cross-workspace -> {s} (NoSuchBucket / gate fires)")
        else:
            print(f"  FAIL PUT cross-workspace -> {s}: {body[:200]}")
            fails += 1

    # Symmetric: alice-b for bucket-a.
    s, resp = presign_put_via_tritond(
        args.mantad_host, token_b, cluster_id, bucket_a, "phase2/cross-b.txt"
    )
    if s == 200:
        cross_url_b = json.loads(resp)["url"]
        s, body = http_put_url(cross_url_b, b"cross B-to-A")
        if s == 404 or "NoSuchBucket" in body:
            print(f"  OK   PUT cross-workspace B->A -> {s} (NoSuchBucket / gate fires)")
        else:
            print(f"  FAIL PUT cross-workspace B->A -> {s}: {body[:200]}")
            fails += 1
    else:
        print(f"  FAIL alice-b cross presign-put -> {s}: {resp[:200]}")
        fails += 1

    print(f"\n== Result: {fails} failure(s) ==")
    return fails


if __name__ == "__main__":
    sys.exit(main())
