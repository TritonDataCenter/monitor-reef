#!/bin/bash
#
# cleanup-404-metadata.sh
#
# Clean up stale metadata entries for objects that got HTTP 404 during
# evacuation. These are objects whose metadata claims they're on the
# evacuated shark (1.stor.coal.joyent.us) but the files don't exist.
#
# Since 1.stor has been wiped, we need to:
#   - For objects with 2+ sharks: remove the 1.stor entry from sharks array
#   - For objects with only 1.stor: delete the metadata entirely (data is lost)
#
# This script runs from the headnode and connects to:
#   1. Rebalancer PostgreSQL (on 3.stor zone) to get 404 object list
#   2. mdapi PostgreSQL (on mdapi zone) to update/delete metadata
#
# Usage:
#   ./cleanup-404-metadata.sh [--dry-run]
#
# Prerequisites:
#   - Run from the headnode or a zone with access to both DBs
#   - REBALANCER_DB_HOST: IP of rebalancer zone (default: localhost)
#   - MDAPI_DB_HOST: IP of mdapi zone (default: autodetected)

set -euo pipefail

EVAC_JOB_ID="06270f5f-224b-463a-b5d5-7937c9860c50"
EVAC_SHARK="1.stor.coal.joyent.us"
DRY_RUN="${1:-}"

REBALANCER_DB_HOST="${REBALANCER_DB_HOST:-localhost}"
MDAPI_DB_HOST="${MDAPI_DB_HOST:-}"

# Auto-detect mdapi zone IP if not set
if [[ -z "$MDAPI_DB_HOST" ]]; then
    # Try to find mdapi zone via SAPI/vmadm
    MDAPI_ZONE=$(vmadm lookup alias=~buckets-mdapi 2>/dev/null | head -1 || true)
    if [[ -n "$MDAPI_ZONE" ]]; then
        MDAPI_DB_HOST=$(vmadm get "$MDAPI_ZONE" | json nics.0.ip 2>/dev/null || true)
    fi
    if [[ -z "$MDAPI_DB_HOST" ]]; then
        echo "ERROR: Cannot auto-detect mdapi zone. Set MDAPI_DB_HOST."
        exit 1
    fi
    echo "Auto-detected mdapi zone at: $MDAPI_DB_HOST"
fi

rebalancer_psql() {
    psql -U postgres -h "$REBALANCER_DB_HOST" -d "$EVAC_JOB_ID" \
        -t -A "$@"
}

mdapi_psql() {
    psql -U postgres -h "$MDAPI_DB_HOST" -d buckets_metadata \
        -t -A "$@"
}

echo "=== Cleanup stale 404 metadata entries ==="
echo "Evacuation job: $EVAC_JOB_ID"
echo "Evacuated shark: $EVAC_SHARK"
echo "Rebalancer DB: $REBALANCER_DB_HOST"
echo "Mdapi DB: $MDAPI_DB_HOST"
[[ "$DRY_RUN" == "--dry-run" ]] && echo "*** DRY RUN MODE ***"
echo ""

# Step 1: Extract 404 objects from the evacuation DB
# The 'object' column is JSONB with the full manta object data
echo "Fetching 404 objects from evacuation DB..."

OBJECTS=$(rebalancer_psql -c "
    SELECT
        id,
        object->>'owner' as owner,
        object->>'bucket_id' as bucket_id,
        object->>'name' as name,
        object->'sharks' as sharks,
        shard
    FROM evacuateobjects
    WHERE skipped_reason LIKE '%404%'
    ORDER BY id;
")

TOTAL=$(echo "$OBJECTS" | grep -c '|' || echo 0)
echo "Found $TOTAL objects with 404 status"
echo ""

UPDATED=0
DELETED=0
ERRORS=0
NOT_FOUND=0

while IFS='|' read -r obj_id owner bucket_id name sharks_json shard; do
    [[ -z "$obj_id" ]] && continue

    echo "Processing: $obj_id (owner=$owner bucket=$bucket_id name=$name shard=$shard)"

    # Parse sharks array to check how many sharks remain after removing EVAC_SHARK
    # sharks_json is like: [{"datacenter":"coal","manta_storage_id":"1.stor.coal.joyent.us"}, ...]
    remaining_sharks=$(echo "$sharks_json" | \
        python -c "
import sys, json
sharks = json.load(sys.stdin)
remaining = [s for s in sharks if s.get('manta_storage_id') != '$EVAC_SHARK']
print(len(remaining))
" 2>/dev/null || echo "error")

    if [[ "$remaining_sharks" == "error" ]]; then
        echo "  ERROR: Failed to parse sharks JSON: $sharks_json"
        ERRORS=$((ERRORS + 1))
        continue
    fi

    # Build the new sharks array (without the evacuated shark)
    new_sharks_pg=$(echo "$sharks_json" | \
        python -c "
import sys, json
sharks = json.load(sys.stdin)
remaining = [s for s in sharks if s.get('manta_storage_id') != '$EVAC_SHARK']
# Format as PostgreSQL text array: {\"json1\",\"json2\"}
parts = []
for s in remaining:
    # Each element is a JSON string, escaped for PG text array
    j = json.dumps(s, separators=(',', ':'))
    # Escape double quotes and backslashes for PG array literal
    j = j.replace('\\\\', '\\\\\\\\').replace('\"', '\\\\\"')
    parts.append('\"' + j + '\"')
print('{' + ','.join(parts) + '}')
" 2>/dev/null)

    if [[ "$remaining_sharks" -eq 0 ]]; then
        # No remaining sharks — data is lost, delete the metadata
        echo "  ACTION: DELETE (no remaining sharks, data lost)"

        if [[ "$DRY_RUN" != "--dry-run" ]]; then
            result=$(mdapi_psql -c "
                DELETE FROM manta_bucket_${shard}.manta_bucket_object
                WHERE owner = '${owner}'::uuid
                  AND bucket_id = '${bucket_id}'::uuid
                  AND name = '${name}';
            " 2>&1) || true

            if echo "$result" | grep -q "DELETE 1"; then
                echo "  OK: Deleted"
                DELETED=$((DELETED + 1))
            elif echo "$result" | grep -q "DELETE 0"; then
                echo "  WARN: Object not found in mdapi (already cleaned?)"
                NOT_FOUND=$((NOT_FOUND + 1))
            else
                echo "  ERROR: $result"
                ERRORS=$((ERRORS + 1))
            fi
        else
            echo "  [DRY RUN] Would delete from manta_bucket_${shard}.manta_bucket_object"
            DELETED=$((DELETED + 1))
        fi

    else
        # Other sharks remain — update the sharks array to remove 1.stor
        echo "  ACTION: UPDATE sharks ($remaining_sharks remaining)"

        if [[ "$DRY_RUN" != "--dry-run" ]]; then
            result=$(mdapi_psql -c "
                UPDATE manta_bucket_${shard}.manta_bucket_object
                SET sharks = '${new_sharks_pg}'::text[],
                    modified = current_timestamp
                WHERE owner = '${owner}'::uuid
                  AND bucket_id = '${bucket_id}'::uuid
                  AND name = '${name}';
            " 2>&1) || true

            if echo "$result" | grep -q "UPDATE 1"; then
                echo "  OK: Updated"
                UPDATED=$((UPDATED + 1))
            elif echo "$result" | grep -q "UPDATE 0"; then
                echo "  WARN: Object not found in mdapi (already cleaned?)"
                NOT_FOUND=$((NOT_FOUND + 1))
            else
                echo "  ERROR: $result"
                ERRORS=$((ERRORS + 1))
            fi
        else
            echo "  [DRY RUN] Would update sharks in manta_bucket_${shard}.manta_bucket_object"
            UPDATED=$((UPDATED + 1))
        fi
    fi

done <<< "$OBJECTS"

echo ""
echo "=== Summary ==="
echo "Total 404 objects: $TOTAL"
echo "Updated (removed 1.stor from sharks): $UPDATED"
echo "Deleted (no remaining sharks): $DELETED"
echo "Not found in mdapi: $NOT_FOUND"
echo "Errors: $ERRORS"
