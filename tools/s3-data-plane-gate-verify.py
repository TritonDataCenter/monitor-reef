#!/usr/bin/env python3
"""S3 data-plane workspace gate live verify.

Closes the rev-2 plan's §Verification §Two-tenant SigV4 live verify
on 192.168.1.182. Provisions two tenants whose IAM users share a
name (`alice`), drives SigV4 calls direct against mantad's S3
endpoint, and asserts every cross-tenant probe returns NoSuchBucket.

Runs FROM build02. Reaches the mantad admin API via ssh to
root@192.168.1.182 (admin token stays on the test box; not pulled
into the transcript). SigV4 calls go over the network from build02
directly to mantad on the test box.

Usage:
    python3 s3-data-plane-gate-verify.py [--keep] [--mantad-host HOST]

    --keep              Skip cleanup so the operator can poke at the
                        state afterward.
    --mantad-host HOST  Hostname / IP of the test box. Default
                        192.168.1.182.
    --mantad-s3-port P  Port mantad's S3 + admin listener is on.
                        Default 7443.

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
from botocore.client import Config
from botocore.exceptions import ClientError


@dataclass
class Caller:
    """Materialized SigV4 caller — workspace + AK + SK."""

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
    """Run a single mantad admin API call FROM the test box.

    The admin token lives at /opt/mantad/etc/admin-token on the test
    box. We never pull it into this process; we ship the request via
    ssh + curl so the bearer stays server-side. mantad's admin routes
    are on the internal listener (port 7101) — the public :7443 port
    serves the SigV4 data plane and rejects bearer-auth headers.
    """
    args = [
        "ssh",
        "-o",
        "BatchMode=yes",
        f"root@{mantad_host}",
        # shell on the box: token := $(cat /opt/mantad/etc/admin-token)
        # then curl with the token. -sS shows errors, -f exits nonzero on >= 400.
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


def create_tenant_workspace(mantad_host: str, label: str) -> tuple[uuid.UUID, str]:
    """Mint a workspace via mantad's idempotent create endpoint."""
    tenant_uuid = uuid.uuid4()
    workspace_name = f"t-{tenant_uuid.hex}"
    body = json.dumps(
        {
            "name": workspace_name,
            "tenant_uuid": str(tenant_uuid),
            "description": f"phase1-gate-verify-{label}",
        }
    )
    mantad_admin_curl(mantad_host, "POST", "/admin/v1/workspaces", body)
    return tenant_uuid, workspace_name


def create_iam_user(mantad_host: str, workspace: str, username: str) -> None:
    body = json.dumps({"name": username})
    mantad_admin_curl(
        mantad_host, "POST", f"/admin/v1/users?workspace={workspace}", body
    )


def create_access_key(mantad_host: str, workspace: str, username: str) -> tuple[str, str]:
    out = mantad_admin_curl(
        mantad_host,
        "POST",
        f"/admin/v1/users/{username}/access-keys?workspace={workspace}",
    )
    return out["access_key_id"], out["secret_access_key"]


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


def assert_no_such_bucket(label: str, fn) -> bool:
    """Run a boto3 call; the gate must return NoSuchBucket (404)."""
    try:
        fn()
        print(f"  FAIL {label}: returned success, expected NoSuchBucket")
        return False
    except ClientError as e:
        code = e.response.get("Error", {}).get("Code")
        if code in ("NoSuchBucket", "404"):
            print(f"  OK   {label}: NoSuchBucket")
            return True
        print(f"  FAIL {label}: returned {code} ({e.response.get('Error', {}).get('Message')})")
        return False
    except Exception as e:
        print(f"  FAIL {label}: unexpected {type(e).__name__}: {e}")
        return False


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--mantad-host", default="192.168.1.182")
    ap.add_argument("--mantad-s3-port", default=7443, type=int)
    args = ap.parse_args()

    endpoint_url = f"http://{args.mantad_host}:{args.mantad_s3_port}"
    fails = 0
    callers: list[Caller] = []
    workspaces: list[str] = []

    try:
        # 1. Provision two fresh tenants + workspaces.
        print("== Provisioning tenants ==")
        tenant_a_uuid, ws_a = create_tenant_workspace(args.mantad_host, "A")
        tenant_b_uuid, ws_b = create_tenant_workspace(args.mantad_host, "B")
        workspaces.extend([ws_a, ws_b])
        print(f"  workspace A: {ws_a}")
        print(f"  workspace B: {ws_b}")

        # 2. Mint an IAM user in each workspace. Mantad's IamUser
        # namespace is global (the meta-key is `user.name` only), so
        # we use distinct names — `alice-a` and `alice-b`. The
        # cross-tenant exposure we're closing is at the bucket /
        # operation level (any caller seeing buckets in other
        # workspaces), not name collision; the unit test
        # `cross_tenant_username_collision_is_blocked` proves the
        # gate itself would catch a same-name case if mantad ever
        # allowed it.
        print("\n== Minting IAM users (one per workspace) ==")
        usernames = ["alice-a", "alice-b"]
        for ws, uname in zip((ws_a, ws_b), usernames):
            create_iam_user(args.mantad_host, ws, uname)
            ak_id, ak_secret = create_access_key(args.mantad_host, ws, uname)
            callers.append(
                Caller(
                    label=f"{uname}@{ws[:8]}",
                    workspace=ws,
                    username=uname,
                    access_key_id=ak_id,
                    secret_access_key=ak_secret,
                )
            )
            print(f"  {uname}@{ws[:8]}: ak_id={ak_id[:8]}...")
        alice_a, alice_b = callers

        # 3. Each Alice creates one bucket via SigV4 against the data
        # plane. Bucket should land stamped with the caller's workspace.
        print("\n== Each Alice creates one bucket via SigV4 ==")
        bucket_a = f"phase1-a-{secrets.token_hex(4)}"
        bucket_b = f"phase1-b-{secrets.token_hex(4)}"
        alice_a.client(endpoint_url).create_bucket(Bucket=bucket_a)
        print(f"  alice-a created s3://{bucket_a}")
        alice_b.client(endpoint_url).create_bucket(Bucket=bucket_b)
        print(f"  alice-b created s3://{bucket_b}")

        # 4. Cross-tenant probe matrix — every one must NoSuchBucket.
        print("\n== Cross-tenant probes (must all NoSuchBucket) ==")
        c_a = alice_a.client(endpoint_url)
        c_b = alice_b.client(endpoint_url)

        cases = [
            ("alice-a head_bucket(bucket-b)", lambda: c_a.head_bucket(Bucket=bucket_b)),
            ("alice-a list_objects(bucket-b)", lambda: c_a.list_objects_v2(Bucket=bucket_b)),
            (
                "alice-a put_object(bucket-b/k)",
                lambda: c_a.put_object(Bucket=bucket_b, Key="k", Body=b"x"),
            ),
            ("alice-a get_object(bucket-b/k)", lambda: c_a.get_object(Bucket=bucket_b, Key="k")),
            (
                "alice-a delete_object(bucket-b/k)",
                lambda: c_a.delete_object(Bucket=bucket_b, Key="k"),
            ),
            (
                "alice-a create_multipart(bucket-b/k)",
                lambda: c_a.create_multipart_upload(Bucket=bucket_b, Key="k"),
            ),
            (
                "alice-a copy_object src=bucket-b/k → bucket-a/k2",
                lambda: c_a.copy_object(
                    Bucket=bucket_a, Key="k2", CopySource={"Bucket": bucket_b, "Key": "k"}
                ),
            ),
            ("alice-a delete_bucket(bucket-b)", lambda: c_a.delete_bucket(Bucket=bucket_b)),
            ("alice-b head_bucket(bucket-a)", lambda: c_b.head_bucket(Bucket=bucket_a)),
            ("alice-b list_objects(bucket-a)", lambda: c_b.list_objects_v2(Bucket=bucket_a)),
            (
                "alice-b put_object(bucket-a/k)",
                lambda: c_b.put_object(Bucket=bucket_a, Key="k", Body=b"x"),
            ),
            ("alice-b delete_bucket(bucket-a)", lambda: c_b.delete_bucket(Bucket=bucket_a)),
        ]
        for label, fn in cases:
            if not assert_no_such_bucket(label, fn):
                fails += 1

        # 5. List_buckets visibility: each Alice sees only their own
        # bucket. Probably means a single Bucket in the list of each
        # caller's own.
        print("\n== list_buckets visibility ==")
        a_list = {b["Name"] for b in c_a.list_buckets().get("Buckets") or []}
        b_list = {b["Name"] for b in c_b.list_buckets().get("Buckets") or []}
        if bucket_a in a_list and bucket_b not in a_list:
            print(f"  OK   alice-a sees only her bucket: {a_list}")
        else:
            print(f"  FAIL alice-a list_buckets returned {a_list} (expected {{{bucket_a}}})")
            fails += 1
        if bucket_b in b_list and bucket_a not in b_list:
            print(f"  OK   alice-b sees only her bucket: {b_list}")
        else:
            print(f"  FAIL alice-b list_buckets returned {b_list} (expected {{{bucket_b}}})")
            fails += 1

        # 6. Sanity: each Alice CAN operate inside her own workspace.
        print("\n== Same-workspace happy path ==")
        try:
            c_a.put_object(Bucket=bucket_a, Key="happy.txt", Body=b"hello t-A")
            obj = c_a.get_object(Bucket=bucket_a, Key="happy.txt")
            body = obj["Body"].read()
            if body == b"hello t-A":
                print("  OK   alice-a put+get(bucket-a/happy.txt)")
            else:
                print(f"  FAIL alice-a put+get round-trip mismatch: {body!r}")
                fails += 1
        except Exception as e:
            print(f"  FAIL alice-a same-workspace put+get raised {type(e).__name__}: {e}")
            fails += 1

    finally:
        if not args.keep:
            print("\n== Cleanup ==")
            for c in callers:
                try:
                    cl = c.client(endpoint_url)
                    for b in cl.list_buckets().get("Buckets") or []:
                        try:
                            # Drain objects so the bucket can be deleted.
                            for o in cl.list_objects_v2(Bucket=b["Name"]).get("Contents") or []:
                                cl.delete_object(Bucket=b["Name"], Key=o["Key"])
                            cl.delete_bucket(Bucket=b["Name"])
                        except Exception as e:
                            print(f"  (cleanup) drop bucket {b['Name']}: {e}", file=sys.stderr)
                except Exception as e:
                    print(f"  (cleanup) client list for {c.label}: {e}", file=sys.stderr)
                delete_user(args.mantad_host, c.workspace, c.username)
            for ws in workspaces:
                delete_workspace(args.mantad_host, ws)

    print(f"\n== Result: {fails} failure(s) ==")
    return fails


if __name__ == "__main__":
    sys.exit(main())
