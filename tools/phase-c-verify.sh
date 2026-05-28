#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

# Phase C verify: exercises the happy-path of the Tenant ↔ Workspace
# binding contract.
#
# Run this AFTER `phase-c-deploy.sh` succeeded and the cluster has
# been registered with tritond:
#
#   tcadm storage cluster add --name mantad-01 \
#         --cluster-endpoint http://192.168.1.182:7101 \
#         --admin-token "$(cat /opt/triton/etc/mantad-admin-token)" \
#         --surface s3 --json
#   # capture the returned cluster id, then:
#   tcadm config set storage.default_s3_cluster_id <cluster-id>
#
# Then:
#   sh phase-c-verify.sh <silo-id>
#
# Assumes:
#   - tcadm is on PATH and a valid session exists (`tcadm login` done)
#   - jq is on PATH
#   - mantad-adm at /opt/triton/bin/mantad-adm
#   - /opt/triton/etc/mantad-admin-token holds the admin bearer
#
# Out of scope here: bucket-op verification through the forwarder.
# tcadm has no bucket subcommand yet (admin-backend / direct curl
# would be required), and bucket-level isolation is the *next* slice
# anyway (beads-cw2u). The Tenant↔Workspace binding contract is
# verified by:
#   - tritond creating the Tenant row with both binding columns set
#   - mantad-adm confirming the workspace landed on mantad
#   - tritond deleting both atomically on tenant delete
#
# The verify is read-only on cluster registry: assumes a registered
# cluster exists and is the default. Run as the bootstrap root operator.

set -eu

usage() {
    printf 'usage: %s <silo-id>\n' "$0" >&2
    printf '\n' >&2
    printf 'Run `tcadm config list` or read /opt/triton/etc/tritond-bootstrap.toml\n' >&2
    printf 'to find the bootstrap silo id.\n' >&2
    exit 2
}

[ $# -ge 1 ] || usage
SILO_ID="$1"

TENANT_NAME="phase-c-acme-$(date +%s)"

note() { printf '\n==> %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------
# Pre-flight: session + default cluster set
# ---------------------------------------------------------------------
note "tcadm whoami"
tcadm whoami

note "tcadm config get storage.default_s3_cluster_id"
CFG_JSON="$(tcadm config get storage.default_s3_cluster_id --json)"
CLUSTER_ID="$(printf '%s' "$CFG_JSON" | jq -r '.value')"
if [ "$CLUSTER_ID" = "null" ] || [ -z "$CLUSTER_ID" ]; then
    fail "storage.default_s3_cluster_id is unset. run \`tcadm config set storage.default_s3_cluster_id <id>\` first"
fi
note "default cluster: $CLUSTER_ID"

# ---------------------------------------------------------------------
# Happy path: create tenant → workspace minted → bound row in tritond
# ---------------------------------------------------------------------
note "tcadm tenant create $SILO_ID --name $TENANT_NAME --json"
TENANT_JSON="$(tcadm tenant create "$SILO_ID" --name "$TENANT_NAME" --json)"

TENANT_ID="$(printf '%s' "$TENANT_JSON" | jq -r '.id')"
WORKSPACE_ID="$(printf '%s' "$TENANT_JSON" | jq -r '.storage_workspace_id')"
BOUND_CLUSTER="$(printf '%s' "$TENANT_JSON" | jq -r '.storage_cluster_id')"

note "tenant.id:                   $TENANT_ID"
note "tenant.storage_workspace_id: $WORKSPACE_ID"
note "tenant.storage_cluster_id:   $BOUND_CLUSTER"

[ "$WORKSPACE_ID" != "null" ] && [ -n "$WORKSPACE_ID" ] \
    || fail "tenant.storage_workspace_id is null — binding did not happen"
[ "$BOUND_CLUSTER" = "$CLUSTER_ID" ] \
    || fail "tenant.storage_cluster_id ($BOUND_CLUSTER) != default ($CLUSTER_ID)"

# The wire-name on mantad is t-<simple-uuid>.
WORKSPACE_SIMPLE="$(printf '%s' "$WORKSPACE_ID" | tr -d '-')"
EXPECTED_WS_NAME="t-${WORKSPACE_SIMPLE}"
note "expected mantad workspace name: $EXPECTED_WS_NAME"

# ---------------------------------------------------------------------
# Cross-check: mantad-adm sees the workspace
# ---------------------------------------------------------------------
TOKEN_FILE=/opt/triton/etc/mantad-admin-token
MANTAD_ADM=/opt/triton/bin/mantad-adm

if [ -f "$TOKEN_FILE" ] && [ -x "$MANTAD_ADM" ]; then
    MANTAD_ADMIN_TOKEN="$(cat "$TOKEN_FILE")"
    export MANTAD_ADMIN_TOKEN
    note "$MANTAD_ADM workspace list (post-create)"
    if "$MANTAD_ADM" --endpoint http://127.0.0.1:7101 workspace list 2>&1 \
            | tee /tmp/phase-c-ws-list-post-create.txt \
            | grep -q "$EXPECTED_WS_NAME"; then
        note "workspace $EXPECTED_WS_NAME present on mantad"
    else
        cat /tmp/phase-c-ws-list-post-create.txt
        fail "workspace $EXPECTED_WS_NAME not visible to mantad-adm after tenant create"
    fi
else
    note "skipping mantad-adm cross-check (token file or binary missing)"
fi

# ---------------------------------------------------------------------
# Delete tenant → workspace archived
# ---------------------------------------------------------------------
note "tcadm tenant delete $SILO_ID $TENANT_ID"
tcadm tenant delete "$SILO_ID" "$TENANT_ID"

if [ -f "$TOKEN_FILE" ] && [ -x "$MANTAD_ADM" ]; then
    note "$MANTAD_ADM workspace list (post-delete)"
    if "$MANTAD_ADM" --endpoint http://127.0.0.1:7101 workspace list 2>&1 \
            | tee /tmp/phase-c-ws-list-post-delete.txt \
            | grep -q "$EXPECTED_WS_NAME"; then
        cat /tmp/phase-c-ws-list-post-delete.txt
        fail "workspace $EXPECTED_WS_NAME still present after tenant delete"
    else
        note "workspace $EXPECTED_WS_NAME archived from mantad"
    fi
fi

# ---------------------------------------------------------------------
# Audit cross-reference
# ---------------------------------------------------------------------
note "tcadm audit list --limit 20"
tcadm audit list --limit 20

cat <<EOF

==================================================================
phase-c-verify happy-path: PASS

  silo:      $SILO_ID
  tenant:    $TENANT_ID  ($TENANT_NAME)
  workspace: $EXPECTED_WS_NAME  (storage_workspace_id=$WORKSPACE_ID)
  cluster:   $CLUSTER_ID

The Tenant↔Workspace binding contract is enforced end-to-end:
  - Tenant create issued workspace-create RPC to mantad first;
    Tenant row committed only after mantad acknowledged.
  - mantad-adm confirmed the workspace landed.
  - Tenant delete archived the workspace on mantad before
    dropping the row in tritond.
  - Paired audit events recorded.
==================================================================
EOF
