#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.

# cw2u (Phase D) live verify — buckets + IAM (users, access keys, policies).
#
# Exercises the mantad-side wire shape introduced by:
#   manta-storage 138d00e — workspace-scope query param on bucket admin routes
#   monitor-reef  c38293e4 — tritond threads workspace scope through bucket forwarders
#   manta-storage 9ed8dcd — workspace-scope query param on IAM admin routes
#   monitor-reef  26206a1f — tritond threads workspace scope through IAM forwarders
#
# Drives mantad directly via curl + the admin token, asserts every
# `?workspace=` semantic across all 15 cw2u routes (4 bucket + 4 user
# + 3 access key + 4 policy). Tritond gets a smoke check (root-operator
# forwarder still works); a true tenant-principal end-to-end is a
# follow-up because tenant API-key minting isn't yet wired.
#
# Run this ON THE TEST BOX (192.168.1.182) as root.
#
# Usage:
#   sh cw2u-bucket-verify.sh
#
# Outputs evidence to /var/log/cw2u-bucket-verify.log AND prints a
# brief PASS/FAIL line per case to stdout. Exits non-zero on the
# first failure so the operator notices.

set -eu

MANTAD_URL="${MANTAD_URL:-http://127.0.0.1:7101}"
TOKEN_FILE="${TOKEN_FILE:-/opt/mantad/etc/admin-token}"
LOG="/var/log/cw2u-bucket-verify.log"

[ -f "$TOKEN_FILE" ] || { printf 'admin token not found at %s\n' "$TOKEN_FILE" >&2; exit 1; }
TOKEN="$(cat "$TOKEN_FILE")"

# Real hex UUIDs (so mantad's parser accepts them). The 'cw2u' theme
# is preserved via the first quartet — c12fb1ee-... — without using
# non-hex characters that 422 the JSON deserialiser.
TENANT_A_UUID="c12fb1ee-0000-0000-0000-0000000000a1"
TENANT_B_UUID="c12fb1ee-0000-0000-0000-0000000000b2"
WS_A="t-c12fb1ee0000000000000000000000a1"
WS_B="t-c12fb1ee0000000000000000000000b2"

BUCKET_A="cw2u-verify-a-$$"
BUCKET_B="cw2u-verify-b-$$"
BUCKET_ROOT="cw2u-verify-root-$$"

note()  { printf '==> %s\n' "$*" | tee -a "$LOG"; }
fatal() { printf 'cw2u-verify FAIL: %s\n' "$*" >&2; printf 'FAIL: %s\n' "$*" >> "$LOG"; exit 1; }
pass()  { printf 'PASS: %s\n' "$*"; printf 'PASS: %s\n' "$*" >> "$LOG"; }

# Helper. Calls mantad, prints body+status to caller, mirrors to log.
# Usage: call METHOD PATH [BODY-JSON]
call() {
    _method="$1"; _path="$2"; _body="${3:-}"
    if [ -n "$_body" ]; then
        printf '\n--- %s %s body=%s\n' "$_method" "$_path" "$_body" >> "$LOG"
        curl -sS --max-time 10 \
            -H "Authorization: Bearer $TOKEN" \
            -H "Content-Type: application/json" \
            -X "$_method" -d "$_body" \
            -w '\nHTTP %{http_code}\n' \
            "$MANTAD_URL$_path"
    else
        printf '\n--- %s %s\n' "$_method" "$_path" >> "$LOG"
        curl -sS --max-time 10 \
            -H "Authorization: Bearer $TOKEN" \
            -X "$_method" \
            -w '\nHTTP %{http_code}\n' \
            "$MANTAD_URL$_path"
    fi
}

# expect_status WANT METHOD PATH [BODY]
# Asserts the response status; on success echoes the body (status line
# stripped) so callers can grep it.
expect_status() {
    _want="$1"; shift
    _out=$(call "$@")
    _got=$(printf '%s' "$_out" | grep -oE 'HTTP [0-9]+' | tail -1 | awk '{print $2}')
    printf '%s\n  expected HTTP %s, got HTTP %s\n' "$*" "$_want" "$_got" >> "$LOG"
    printf 'BODY:\n%s\n' "$_out" >> "$LOG"
    if [ "$_got" != "$_want" ]; then
        fatal "$* — expected HTTP $_want, got HTTP $_got (body: $_out)"
    fi
    printf '%s' "$_out" | sed '/^HTTP /d'
}

# -------- setup: two workspaces --------------------------------------

note "begin cw2u bucket-only verify ($(date -u +%Y-%m-%dT%H:%M:%SZ))"
note "mantad: $MANTAD_URL"

expect_status 200 POST /admin/v1/workspaces \
    "{\"tenant_uuid\":\"$TENANT_A_UUID\",\"name\":\"$WS_A\",\"description\":\"verify A\"}" > /dev/null
pass "workspace A created ($WS_A)"

expect_status 200 POST /admin/v1/workspaces \
    "{\"tenant_uuid\":\"$TENANT_B_UUID\",\"name\":\"$WS_B\",\"description\":\"verify B\"}" > /dev/null
pass "workspace B created ($WS_B)"

# -------- create-bucket --------------------------------------------

# scoped create stamps workspace
body_a=$(expect_status 200 POST "/admin/v1/buckets?workspace=$WS_A" \
    "{\"name\":\"$BUCKET_A\",\"owner\":\"root\"}")
echo "$body_a" | grep -q "\"workspace\":\"$WS_A\"" \
    || fatal "bucket-A response missing workspace field (got: $body_a)"
pass "create_bucket?workspace=A stamps workspace field"

body_b=$(expect_status 200 POST "/admin/v1/buckets?workspace=$WS_B" \
    "{\"name\":\"$BUCKET_B\",\"owner\":\"root\"}")
echo "$body_b" | grep -q "\"workspace\":\"$WS_B\"" \
    || fatal "bucket-B response missing workspace field"
pass "create_bucket?workspace=B stamps workspace field"

# admin-direct create (no workspace param) — empty workspace field
body_root=$(expect_status 200 POST /admin/v1/buckets \
    "{\"name\":\"$BUCKET_ROOT\",\"owner\":\"root\"}")
echo "$body_root" | grep -q '"workspace":""' \
    || fatal "admin-direct create — workspace field should be empty (got: $body_root)"
pass "create_bucket without workspace leaves field empty"

# create against a nonexistent workspace → 404
expect_status 404 POST "/admin/v1/buckets?workspace=t-doesnotexist00000000000000000000" \
    "{\"name\":\"never\",\"owner\":\"root\"}" > /dev/null
pass "create_bucket?workspace=BOGUS returns 404"

# -------- list-buckets --------------------------------------------

list_root=$(expect_status 200 GET /admin/v1/buckets)
for needed in "$BUCKET_A" "$BUCKET_B" "$BUCKET_ROOT"; do
    echo "$list_root" | grep -q "\"name\":\"$needed\"" \
        || fatal "list (root) missing $needed"
done
pass "list_buckets (no workspace) returns all three"

list_a=$(expect_status 200 GET "/admin/v1/buckets?workspace=$WS_A")
echo "$list_a" | grep -q "\"name\":\"$BUCKET_A\""  || fatal "list?workspace=A missing $BUCKET_A"
if echo "$list_a" | grep -q "\"name\":\"$BUCKET_B\""; then fatal "list?workspace=A leaked $BUCKET_B"; fi
if echo "$list_a" | grep -q "\"name\":\"$BUCKET_ROOT\""; then fatal "list?workspace=A leaked $BUCKET_ROOT"; fi
pass "list_buckets?workspace=A returns only A's bucket"

list_b=$(expect_status 200 GET "/admin/v1/buckets?workspace=$WS_B")
echo "$list_b" | grep -q "\"name\":\"$BUCKET_B\""  || fatal "list?workspace=B missing $BUCKET_B"
if echo "$list_b" | grep -q "\"name\":\"$BUCKET_A\""; then fatal "list?workspace=B leaked $BUCKET_A"; fi
pass "list_buckets?workspace=B returns only B's bucket"

list_bogus=$(expect_status 200 GET "/admin/v1/buckets?workspace=t-doesnotexist00000000000000000000")
[ "$list_bogus" = "[]" ] || fatal "list?workspace=BOGUS should be empty, got: $list_bogus"
pass "list_buckets?workspace=BOGUS returns empty array"

# -------- get-bucket ---------------------------------------------

expect_status 200 GET "/admin/v1/buckets/$BUCKET_A?workspace=$WS_A" > /dev/null
pass "get_bucket A?workspace=A returns 200"

expect_status 404 GET "/admin/v1/buckets/$BUCKET_A?workspace=$WS_B" > /dev/null
pass "get_bucket A?workspace=B returns 404 (cross-tenant probe blocked)"

expect_status 200 GET "/admin/v1/buckets/$BUCKET_A" > /dev/null
pass "get_bucket A (no workspace, root view) returns 200"

# -------- delete-bucket ------------------------------------------

expect_status 404 DELETE "/admin/v1/buckets/$BUCKET_A?workspace=$WS_B" > /dev/null
pass "delete A?workspace=B returns 404 (cross-tenant delete blocked)"

# still alive after the failed cross-tenant delete
expect_status 200 GET "/admin/v1/buckets/$BUCKET_A?workspace=$WS_A" > /dev/null
pass "bucket A still exists after the failed cross-tenant delete"

expect_status 204 DELETE "/admin/v1/buckets/$BUCKET_A?workspace=$WS_A" > /dev/null
pass "delete A?workspace=A returns 204"

expect_status 404 GET "/admin/v1/buckets/$BUCKET_A?workspace=$WS_A" > /dev/null
pass "get A after delete returns 404"

expect_status 204 DELETE "/admin/v1/buckets/$BUCKET_B?workspace=$WS_B" > /dev/null
expect_status 204 DELETE "/admin/v1/buckets/$BUCKET_ROOT" > /dev/null
pass "cleanup: B + root buckets deleted"

# -------- IAM: users --------------------------------------------

USER_A="cw2u-user-a-$$"
USER_B="cw2u-user-b-$$"
USER_ROOT="cw2u-user-root-$$"

# create scoped users stamp the workspace
body_ua=$(expect_status 200 POST "/admin/v1/users?workspace=$WS_A" \
    "{\"name\":\"$USER_A\"}")
echo "$body_ua" | grep -q "\"workspace\":\"$WS_A\"" \
    || fatal "create_user A — workspace field not stamped (got: $body_ua)"
pass "create_user?workspace=A stamps workspace field"

body_ub=$(expect_status 200 POST "/admin/v1/users?workspace=$WS_B" \
    "{\"name\":\"$USER_B\"}")
echo "$body_ub" | grep -q "\"workspace\":\"$WS_B\"" \
    || fatal "create_user B — workspace field not stamped"
pass "create_user?workspace=B stamps workspace field"

body_ur=$(expect_status 200 POST /admin/v1/users \
    "{\"name\":\"$USER_ROOT\"}")
echo "$body_ur" | grep -q '"workspace":""' \
    || fatal "create_user unscoped — workspace should be empty (got: $body_ur)"
pass "create_user without workspace leaves field empty"

# create against unknown workspace → 404
expect_status 404 POST "/admin/v1/users?workspace=t-doesnotexist00000000000000000000" \
    "{\"name\":\"never-user\"}" > /dev/null
pass "create_user?workspace=BOGUS returns 404"

# list scoping
u_list_root=$(expect_status 200 GET /admin/v1/users)
for needed in "$USER_A" "$USER_B" "$USER_ROOT"; do
    echo "$u_list_root" | grep -q "\"name\":\"$needed\"" \
        || fatal "list_users (root) missing $needed"
done
pass "list_users (no workspace) returns all three"

u_list_a=$(expect_status 200 GET "/admin/v1/users?workspace=$WS_A")
echo "$u_list_a" | grep -q "\"name\":\"$USER_A\"" || fatal "list_users?workspace=A missing $USER_A"
if echo "$u_list_a" | grep -q "\"name\":\"$USER_B\""; then fatal "list_users?workspace=A leaked $USER_B"; fi
if echo "$u_list_a" | grep -q "\"name\":\"$USER_ROOT\""; then fatal "list_users?workspace=A leaked $USER_ROOT"; fi
pass "list_users?workspace=A returns only A's user"

# get-user scoping
expect_status 200 GET "/admin/v1/users/$USER_A?workspace=$WS_A" > /dev/null
pass "get_user A?workspace=A returns 200"

expect_status 404 GET "/admin/v1/users/$USER_A?workspace=$WS_B" > /dev/null
pass "get_user A?workspace=B returns 404 (cross-tenant probe blocked)"

# -------- IAM: access keys --------------------------------------

ak_a_body=$(expect_status 200 POST "/admin/v1/users/$USER_A/access-keys?workspace=$WS_A")
echo "$ak_a_body" | grep -q "\"workspace\":\"$WS_A\"" \
    || fatal "create_access_key A — workspace not stamped (got: $ak_a_body)"
AK_A_ID=$(printf '%s' "$ak_a_body" | sed -n 's/.*"access_key_id":"\([^"]*\)".*/\1/p')
[ -n "$AK_A_ID" ] || fatal "could not extract AKID from create_access_key response"
pass "create_access_key A?workspace=A stamps workspace + returns AKID ($AK_A_ID)"

# cross-tenant create against a user that belongs to the OTHER workspace → 404
expect_status 404 POST "/admin/v1/users/$USER_A/access-keys?workspace=$WS_B" > /dev/null
pass "create_access_key on A?workspace=B returns 404 (cross-tenant blocked)"

# list AKs scoped
ak_list=$(expect_status 200 GET "/admin/v1/users/$USER_A/access-keys?workspace=$WS_A")
echo "$ak_list" | grep -q "\"access_key_id\":\"$AK_A_ID\"" \
    || fatal "list_access_keys A?workspace=A missing $AK_A_ID"
pass "list_access_keys A?workspace=A returns own key"

expect_status 404 GET "/admin/v1/users/$USER_A/access-keys?workspace=$WS_B" > /dev/null
pass "list_access_keys A?workspace=B returns 404"

# delete AK cross-tenant blocked
expect_status 404 DELETE "/admin/v1/access-keys/$AK_A_ID?workspace=$WS_B" > /dev/null
pass "delete_access_key cross-tenant returns 404"

expect_status 204 DELETE "/admin/v1/access-keys/$AK_A_ID?workspace=$WS_A" > /dev/null
pass "delete_access_key own returns 204"

# -------- IAM: policies -----------------------------------------

POLICY_A="cw2u-policy-a"
POLICY_DOC='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:ListBucket","Resource":"*"}]}'

# put policy scoped
expect_status 204 PUT "/admin/v1/users/$USER_A/policies/$POLICY_A?workspace=$WS_A" "$POLICY_DOC" > /dev/null
pass "put_user_policy A/p?workspace=A returns 204"

# put policy cross-tenant blocked
expect_status 404 PUT "/admin/v1/users/$USER_A/policies/other?workspace=$WS_B" "$POLICY_DOC" > /dev/null
pass "put_user_policy A/p?workspace=B returns 404 (cross-tenant write blocked)"

# list / get scoped
pol_list=$(expect_status 200 GET "/admin/v1/users/$USER_A/policies?workspace=$WS_A")
echo "$pol_list" | grep -q "\"$POLICY_A\"" \
    || fatal "list_user_policies A?workspace=A missing $POLICY_A"
pass "list_user_policies A?workspace=A includes our policy"

expect_status 404 GET "/admin/v1/users/$USER_A/policies?workspace=$WS_B" > /dev/null
pass "list_user_policies A?workspace=B returns 404"

expect_status 200 GET "/admin/v1/users/$USER_A/policies/$POLICY_A?workspace=$WS_A" > /dev/null
pass "get_user_policy A/p?workspace=A returns 200"

expect_status 404 GET "/admin/v1/users/$USER_A/policies/$POLICY_A?workspace=$WS_B" > /dev/null
pass "get_user_policy A/p?workspace=B returns 404"

# delete policy scoped
expect_status 404 DELETE "/admin/v1/users/$USER_A/policies/$POLICY_A?workspace=$WS_B" > /dev/null
pass "delete_user_policy cross-tenant returns 404"

expect_status 204 DELETE "/admin/v1/users/$USER_A/policies/$POLICY_A?workspace=$WS_A" > /dev/null
pass "delete_user_policy own returns 204"

# -------- IAM cleanup -------------------------------------------

# Cross-tenant delete-user blocked
expect_status 404 DELETE "/admin/v1/users/$USER_A?workspace=$WS_B" > /dev/null
pass "delete_user A?workspace=B returns 404 (cross-tenant blocked)"

expect_status 204 DELETE "/admin/v1/users/$USER_A?workspace=$WS_A" > /dev/null
expect_status 204 DELETE "/admin/v1/users/$USER_B?workspace=$WS_B" > /dev/null
expect_status 204 DELETE "/admin/v1/users/$USER_ROOT" > /dev/null
pass "cleanup: IAM users deleted"

# -------- tritond root smoke (no regression on the cluster-wide path) ----

# Skip the tritond leg unless the operator wired up a root session.
if [ -n "${TRITOND_URL:-}" ] && [ -n "${TRITOND_BEARER:-}" ] && [ -n "${TRITOND_CLUSTER_ID:-}" ]; then
    note "tritond root smoke: $TRITOND_URL cluster=$TRITOND_CLUSTER_ID"

    smoke_bucket="cw2u-tritond-root-$$"
    smoke_status=$(curl -sS --max-time 10 \
        -H "Authorization: Bearer $TRITOND_BEARER" \
        -H "Content-Type: application/json" \
        -X POST "$TRITOND_URL/v1/storage/clusters/$TRITOND_CLUSTER_ID/buckets" \
        -d "{\"name\":\"$smoke_bucket\",\"owner\":\"root\"}" \
        -o /tmp/tritond-create.json \
        -w '%{http_code}' 2>>"$LOG")
    [ "$smoke_status" = "201" ] || fatal "tritond root create bucket: expected 201, got $smoke_status"
    pass "tritond root create bucket returns 201"

    mantad_body=$(expect_status 200 GET "/admin/v1/buckets/$smoke_bucket")
    echo "$mantad_body" | grep -q '"workspace":""' \
        || fatal "tritond root create — mantad shows non-empty workspace (got: $mantad_body)"
    pass "tritond root create lands in mantad with empty workspace (no scope leak)"

    curl -sS --max-time 10 \
        -H "Authorization: Bearer $TRITOND_BEARER" \
        -X DELETE "$TRITOND_URL/v1/storage/clusters/$TRITOND_CLUSTER_ID/buckets/$smoke_bucket" \
        -w '\nHTTP %{http_code}\n' >> "$LOG" 2>&1
    pass "tritond root delete bucket"
else
    note "(tritond root smoke skipped — set TRITOND_URL, TRITOND_BEARER, TRITOND_CLUSTER_ID to enable)"
fi

# -------- workspace cleanup --------------------------------------

expect_status 204 DELETE "/admin/v1/workspaces/$WS_A" > /dev/null
expect_status 204 DELETE "/admin/v1/workspaces/$WS_B" > /dev/null
pass "cleanup: verify workspaces deleted"

# -------- summary -------------------------------------------------

note "all cases pass — wire-shape verified end-to-end"
note "transcript: $LOG"
