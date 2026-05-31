#!/usr/bin/env python3
"""Phase 2: per-workspace presigner credentials live verify.

Closes the rev-2 plan §Phase 2 verify items 9–11:
  9. Presigned PUT/GET signed with the per-workspace presigner key
     succeeds inside the workspace.
  10. The same URL rewritten to point at a bucket in another
     workspace fails (NoSuchBucket; the Phase 1 gate fires because
     the presigner now resolves to `Iam { workspace = t-A }` rather
     than root).
  11. Idempotency: a second `provision_workspace_presigner` call
     returns the same `(access_key_id, secret_access_key)`.

Runs from build02; reaches mantad's admin API via ssh+curl to root@
192.168.1.182 (admin token stays server-side). SigV4 signing of the
presigned URL uses boto3.

Usage:
    python3 s3-presigner-workspace-verify.py [--keep]
                                             [--mantad-host HOST]

Exit code is the number of failed assertions.
"""

import argparse
import json
import secrets
import subprocess
import sys
import uuid
from dataclasses import dataclass

import boto3
import urllib.request
from botocore.client import Config
from botocore.exceptions import ClientError


@dataclass
class Caller:
    label: str
    workspace: str
    username: str
    access_key_id: str
    secret_access_key: str

    def client(self, endpoint_url: str):
        return boto3.client(
            "s3",
            endpoint_url=endpoint_url,
            aws_access_key_id=self.access_key_id,
            aws_secret_access_key=self.secret_access_key,
            region_name="us-east-1",
            config=Config(signature_version="s3v4", s3={"addressing_style": "path"}),
        )


def mantad_admin_curl(mantad_host: str, method: str, path: str, body: str | None = None) -> dict:
    args = [
        "ssh",
        "-o",
        "BatchMode=yes",
        f"root@{mantad_host}",
        f"token=$(cat /opt/mantad/etc/admin-token); curl -sS -f -X {method}"
        f" -H \"Authorization: Bearer $token\""
        + (' -H "Content-Type: application/json"' if body else "")
        + (f" -d {json.dumps(body)}" if body else "")
        + f" http://127.0.0.1:7101{path}",
    ]
    p = subprocess.run(args, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(
            f"mantad admin {method} {path} failed (rc={p.returncode}):\n"
            f"stderr: {p.stderr}\nstdout: {p.stdout}"
        )
    if not p.stdout.strip():
        return {}
    return json.loads(p.stdout)


def provision_workspace(mantad_host: str, label: str) -> tuple[uuid.UUID, str]:
    tenant_uuid = uuid.uuid4()
    workspace_name = f"t-{tenant_uuid.hex}"
    mantad_admin_curl(
        mantad_host,
        "POST",
        "/admin/v1/workspaces",
        json.dumps(
            {
                "name": workspace_name,
                "tenant_uuid": str(tenant_uuid),
                "description": f"phase2-presigner-verify-{label}",
            }
        ),
    )
    return tenant_uuid, workspace_name


def provision_iam(mantad_host: str, workspace: str, username: str) -> tuple[str, str]:
    mantad_admin_curl(
        mantad_host,
        "POST",
        f"/admin/v1/users?workspace={workspace}",
        json.dumps({"name": username}),
    )
    out = mantad_admin_curl(
        mantad_host,
        "POST",
        f"/admin/v1/users/{username}/access-keys?workspace={workspace}",
    )
    return out["access_key_id"], out["secret_access_key"]


def provision_presigner(mantad_host: str, workspace: str) -> dict:
    return mantad_admin_curl(
        mantad_host,
        "POST",
        f"/admin/v1/workspaces/{workspace}/presigner",
    )


def delete_workspace(mantad_host: str, workspace: str) -> None:
    try:
        mantad_admin_curl(mantad_host, "DELETE", f"/admin/v1/workspaces/{workspace}")
    except Exception as e:
        print(f"  (cleanup) delete workspace {workspace} failed: {e}", file=sys.stderr)


def delete_user(mantad_host: str, workspace: str, username: str) -> None:
    try:
        mantad_admin_curl(
            mantad_host,
            "DELETE",
            f"/admin/v1/users/{username}?workspace={workspace}",
        )
    except Exception as e:
        print(f"  (cleanup) delete user {username}@{workspace} failed: {e}", file=sys.stderr)


def http_put(url: str, body: bytes) -> tuple[int, str]:
    req = urllib.request.Request(url, data=body, method="PUT")
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def http_get(url: str) -> tuple[int, str]:
    try:
        with urllib.request.urlopen(url) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--mantad-host", default="192.168.1.182")
    ap.add_argument("--mantad-s3-port", default=7443, type=int)
    args = ap.parse_args()

    endpoint_url = f"http://{args.mantad_host}:{args.mantad_s3_port}"
    fails = 0
    iam_callers: list[Caller] = []
    workspaces: list[str] = []

    try:
        # 1. Two fresh workspaces, each with an IAM user + bucket.
        print("== Provisioning tenants + IAM users + buckets ==")
        _, ws_a = provision_workspace(args.mantad_host, "A")
        _, ws_b = provision_workspace(args.mantad_host, "B")
        workspaces.extend([ws_a, ws_b])
        print(f"  workspace A: {ws_a}")
        print(f"  workspace B: {ws_b}")

        for ws, uname in zip((ws_a, ws_b), ("alice-a-p2", "alice-b-p2")):
            ak, sk = provision_iam(args.mantad_host, ws, uname)
            iam_callers.append(
                Caller(
                    label=f"{uname}@{ws[:8]}",
                    workspace=ws,
                    username=uname,
                    access_key_id=ak,
                    secret_access_key=sk,
                )
            )

        alice_a, alice_b = iam_callers
        bucket_a = f"phase2-a-{secrets.token_hex(4)}"
        bucket_b = f"phase2-b-{secrets.token_hex(4)}"
        alice_a.client(endpoint_url).create_bucket(Bucket=bucket_a)
        alice_b.client(endpoint_url).create_bucket(Bucket=bucket_b)
        print(f"  alice_a created s3://{bucket_a}")
        print(f"  alice_b created s3://{bucket_b}")

        # 2. Provision per-workspace presigner credentials. Verify
        # idempotency (item 11).
        print("\n== Provisioning per-workspace presigner credentials ==")
        presigner_a_v1 = provision_presigner(args.mantad_host, ws_a)
        presigner_a_v2 = provision_presigner(args.mantad_host, ws_a)
        presigner_b = provision_presigner(args.mantad_host, ws_b)
        print(
            f"  presigner@{ws_a[:8]}: user={presigner_a_v1['user']} "
            f"workspace={presigner_a_v1['workspace']} ak={presigner_a_v1['access_key_id'][:8]}..."
        )
        print(
            f"  presigner@{ws_b[:8]}: user={presigner_b['user']} "
            f"workspace={presigner_b['workspace']} ak={presigner_b['access_key_id'][:8]}..."
        )

        if (
            presigner_a_v1["access_key_id"] == presigner_a_v2["access_key_id"]
            and presigner_a_v1["secret_access_key"] == presigner_a_v2["secret_access_key"]
        ):
            print("  OK   idempotent: 2nd call returns the same AK + secret")
        else:
            print(
                f"  FAIL idempotency: v1 ak={presigner_a_v1['access_key_id']} "
                f"vs v2 ak={presigner_a_v2['access_key_id']}"
            )
            fails += 1

        if presigner_a_v1["workspace"] != ws_a:
            print(
                f"  FAIL presigner A workspace mismatch: got {presigner_a_v1['workspace']} "
                f"expected {ws_a}"
            )
            fails += 1
        if presigner_b["workspace"] != ws_b:
            print(
                f"  FAIL presigner B workspace mismatch: got {presigner_b['workspace']} "
                f"expected {ws_b}"
            )
            fails += 1
        if not presigner_a_v1["user"].startswith("presigner-"):
            print(f"  FAIL presigner A user not system-tagged: {presigner_a_v1['user']}")
            fails += 1

        # 3. Use the per-workspace presigner cred to SIGN a URL for
        # bucket-a (in-workspace). Should succeed.
        print("\n== Presigned PUT signed with per-workspace key — in-workspace ==")
        presigner_a_client = boto3.client(
            "s3",
            endpoint_url=endpoint_url,
            aws_access_key_id=presigner_a_v1["access_key_id"],
            aws_secret_access_key=presigner_a_v1["secret_access_key"],
            region_name="us-east-1",
            config=Config(signature_version="s3v4", s3={"addressing_style": "path"}),
        )
        good_url = presigner_a_client.generate_presigned_url(
            "put_object",
            Params={"Bucket": bucket_a, "Key": "phase2/probe.txt"},
            ExpiresIn=300,
        )
        status, body = http_put(good_url, b"phase2 hello from in-workspace presign")
        if status == 200:
            print(f"  OK   PUT {bucket_a}/phase2/probe.txt via per-ws presign → 200")
        else:
            print(f"  FAIL PUT in-workspace returned {status}: {body[:160]}")
            fails += 1

        # 4. Cross-workspace URL rewrite: take the SAME presign style
        # and sign for bucket-b. Must fail at mantad (NoSuchBucket
        # via the Phase 1 gate, because the signing identity is now
        # presigner-t-A which is NOT in t-B).
        print("\n== Presigned PUT cross-workspace — must NoSuchBucket ==")
        cross_url = presigner_a_client.generate_presigned_url(
            "put_object",
            Params={"Bucket": bucket_b, "Key": "phase2/cross.txt"},
            ExpiresIn=300,
        )
        status, body = http_put(cross_url, b"cross-tenant attempt")
        if "NoSuchBucket" in body or status == 404:
            print(f"  OK   PUT bucket-b via t-A presigner blocked (status={status})")
        else:
            print(f"  FAIL cross-workspace PUT returned {status}: {body[:200]}")
            fails += 1

        # 5. Symmetry: t-B presigner signing for bucket-a must also
        # fail.
        print("\n== Presigned PUT cross-workspace (B→A) — must NoSuchBucket ==")
        presigner_b_client = boto3.client(
            "s3",
            endpoint_url=endpoint_url,
            aws_access_key_id=presigner_b["access_key_id"],
            aws_secret_access_key=presigner_b["secret_access_key"],
            region_name="us-east-1",
            config=Config(signature_version="s3v4", s3={"addressing_style": "path"}),
        )
        cross_b_url = presigner_b_client.generate_presigned_url(
            "put_object",
            Params={"Bucket": bucket_a, "Key": "phase2/cross-b.txt"},
            ExpiresIn=300,
        )
        status, body = http_put(cross_b_url, b"cross-tenant attempt B→A")
        if "NoSuchBucket" in body or status == 404:
            print(f"  OK   PUT bucket-a via t-B presigner blocked (status={status})")
        else:
            print(f"  FAIL cross-workspace PUT B→A returned {status}: {body[:200]}")
            fails += 1

        # 6. Presigned GET inside the workspace round-trips.
        print("\n== Presigned GET — in-workspace round-trip ==")
        get_url = presigner_a_client.generate_presigned_url(
            "get_object",
            Params={"Bucket": bucket_a, "Key": "phase2/probe.txt"},
            ExpiresIn=300,
        )
        status, body = http_get(get_url)
        if status == 200 and body == "phase2 hello from in-workspace presign":
            print(f"  OK   GET {bucket_a}/phase2/probe.txt via per-ws presign → 200 + correct body")
        else:
            print(f"  FAIL GET in-workspace returned {status}: {body[:160]}")
            fails += 1

    finally:
        if not args.keep:
            print("\n== Cleanup ==")
            for c in iam_callers:
                try:
                    cl = c.client(endpoint_url)
                    for b in cl.list_buckets().get("Buckets") or []:
                        try:
                            for o in cl.list_objects_v2(Bucket=b["Name"]).get("Contents") or []:
                                cl.delete_object(Bucket=b["Name"], Key=o["Key"])
                            cl.delete_bucket(Bucket=b["Name"])
                        except Exception as e:
                            print(f"  (cleanup) drop bucket {b['Name']}: {e}", file=sys.stderr)
                except Exception as e:
                    print(f"  (cleanup) client list for {c.label}: {e}", file=sys.stderr)
                delete_user(args.mantad_host, c.workspace, c.username)
            for ws in workspaces:
                # System presigner user is cascaded by workspace delete.
                delete_workspace(args.mantad_host, ws)

    print(f"\n== Result: {fails} failure(s) ==")
    return fails


if __name__ == "__main__":
    sys.exit(main())
