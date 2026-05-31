#!/usr/bin/env python3
"""Phase 3: mantad-adm bucket repair-workspace live verify.

Creates a legacy bucket (workspace = ""), proves it's invisible to
a workspace-bound IAM caller, then stamps the workspace with
`mantad-adm bucket repair-workspace` and proves visibility is
restored.

Runs from build02. Reaches mantad's admin API via ssh+curl to
root@192.168.1.182. The mantad-adm binary is invoked locally on
build02 (it has network access to the test box's admin port).

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
                "description": f"phase3-repair-verify-{label}",
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


def head_bucket_admin(mantad_host: str, bucket: str) -> dict:
    return mantad_admin_curl(
        mantad_host, "GET", f"/admin/v1/buckets/{bucket}"
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


def mantad_adm_repair(bucket: str, workspace: str, mantad_host: str) -> str:
    """Run `mantad-adm bucket repair-workspace`. Returns stdout."""
    args = [
        # mantad-adm is bundled in ~build/Triton-S3/manta-storage/target/release/mantad-adm
        # on build02. Pass --admin-url so we don't need the /etc/peers spec.
        "/home/build/Triton-S3/manta-storage/target/release/mantad-adm",
        "--admin-url",
        f"http://{mantad_host}:7101",
        "--admin-token",
        # Read the admin token from the test box and inline it.
        subprocess.run(
            ["ssh", f"root@{mantad_host}", "cat", "/opt/mantad/etc/admin-token"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip(),
        "bucket",
        "repair-workspace",
        "--bucket",
        bucket,
        "--workspace",
        workspace,
    ]
    p = subprocess.run(args, capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(
            f"mantad-adm bucket repair-workspace failed (rc={p.returncode}):\n"
            f"stderr: {p.stderr}\nstdout: {p.stdout}"
        )
    return p.stdout


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
    legacy_bucket = None

    try:
        # 1. Provision the target workspace + an IAM user inside it.
        print("== Provisioning target workspace + IAM caller ==")
        _, ws = provision_workspace(args.mantad_host, "repair")
        workspaces.append(ws)
        ak, sk = provision_iam(args.mantad_host, ws, "alice-repair")
        alice = Caller(
            label=f"alice@{ws[:8]}",
            workspace=ws,
            username="alice-repair",
            access_key_id=ak,
            secret_access_key=sk,
        )
        callers.append(alice)
        print(f"  workspace: {ws}")
        print(f"  alice ak={ak[:8]}...")

        # 2. Simulate the pre-Phase-1 cohort: create a bucket via
        # the admin API with NO workspace param. Bucket lands with
        # `workspace = ""`. Add a single object so the gate failure
        # cases hit head_object after head_bucket_for.
        legacy_bucket = f"phase3-legacy-{secrets.token_hex(4)}"
        print(f"\n== Creating legacy bucket {legacy_bucket} (workspace=\"\") ==")
        mantad_admin_curl(
            args.mantad_host,
            "POST",
            "/admin/v1/buckets",
            json.dumps({"name": legacy_bucket, "owner": "alice-repair"}),
        )
        admin_view = head_bucket_admin(args.mantad_host, legacy_bucket)
        if admin_view.get("workspace") == "":
            print(f"  OK   admin view: workspace=\"\"  owner={admin_view['owner']}")
        else:
            print(f"  FAIL admin view returned workspace={admin_view.get('workspace')!r}")
            fails += 1

        # 3. With the IAM caller alice, attempt to list / head the
        # legacy bucket. Both must NoSuchBucket: the workspace gate
        # rejects because bucket.workspace="" != caller.workspace.
        print("\n== Pre-repair: alice cannot see the legacy bucket ==")
        c = alice.client(endpoint_url)
        try:
            c.head_bucket(Bucket=legacy_bucket)
            print("  FAIL alice.head_bucket succeeded before repair")
            fails += 1
        except ClientError as e:
            code = e.response.get("Error", {}).get("Code")
            if code in ("NoSuchBucket", "404"):
                print(f"  OK   alice.head_bucket({legacy_bucket}) -> NoSuchBucket")
            else:
                print(f"  FAIL alice.head_bucket returned {code}")
                fails += 1

        # 4. Run mantad-adm bucket repair-workspace to stamp the bucket.
        print(f"\n== Running mantad-adm bucket repair-workspace --workspace {ws[:16]}... ==")
        out = mantad_adm_repair(legacy_bucket, ws, args.mantad_host)
        print(f"  mantad-adm stdout: {out.strip()}")

        # 5. Verify the bucket's admin view now shows the new workspace.
        admin_view2 = head_bucket_admin(args.mantad_host, legacy_bucket)
        if admin_view2.get("workspace") == ws:
            print(f"  OK   admin view post-repair: workspace={ws}")
        else:
            print(f"  FAIL admin view post-repair: workspace={admin_view2.get('workspace')!r}")
            fails += 1

        # 6. Now alice CAN see + operate on the bucket — workspace gate
        # admits, ACL fallback admits (alice is the bucket's owner).
        print("\n== Post-repair: alice can now operate on the bucket ==")
        try:
            c.head_bucket(Bucket=legacy_bucket)
            print(f"  OK   alice.head_bucket({legacy_bucket}) post-repair -> 200")
        except ClientError as e:
            print(f"  FAIL alice.head_bucket post-repair failed: {e.response.get('Error')}")
            fails += 1
        try:
            c.put_object(Bucket=legacy_bucket, Key="repair-probe.txt", Body=b"hi")
            obj = c.get_object(Bucket=legacy_bucket, Key="repair-probe.txt")
            if obj["Body"].read() == b"hi":
                print(f"  OK   alice.put+get(repair-probe.txt) round-trip")
            else:
                print(f"  FAIL alice round-trip body mismatch")
                fails += 1
        except ClientError as e:
            print(f"  FAIL alice put/get post-repair: {e.response.get('Error')}")
            fails += 1

        # 7. Idempotency: re-running the repair against the same
        # workspace is a no-op.
        print("\n== Idempotency: rerun repair against the same workspace ==")
        out2 = mantad_adm_repair(legacy_bucket, ws, args.mantad_host)
        admin_view3 = head_bucket_admin(args.mantad_host, legacy_bucket)
        if admin_view3.get("workspace") == ws:
            print("  OK   idempotent: workspace unchanged on 2nd repair")
        else:
            print(f"  FAIL idempotent repair: now {admin_view3.get('workspace')!r}")
            fails += 1

    finally:
        if not args.keep:
            print("\n== Cleanup ==")
            for c in callers:
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
            for ws_name in workspaces:
                delete_workspace(args.mantad_host, ws_name)

    print(f"\n== Result: {fails} failure(s) ==")
    return fails


if __name__ == "__main__":
    sys.exit(main())
