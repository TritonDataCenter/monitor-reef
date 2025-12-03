#!/bin/bash
# Fetch public issue data for jira-stub-server fixtures using bugview-cli
#
# This script fetches issue data from a running bugview instance and saves it
# in the format expected by jira-stub-server. The fixtures only include data
# that is publicly visible through bugview.
#
# Usage:
#   ./fetch-issues.sh                     # Fetch from production (smartos.org)
#   ./fetch-issues.sh http://localhost:3000  # Fetch from local instance
#
# To add a new fixture:
#   bugview --base-url <url> fetch-fixture <ISSUE-KEY> > <ISSUE-KEY>.json

set -e

BUGVIEW_URL="${1:-https://smartos.org}"
FIXTURES_DIR="$(dirname "$0")"

# Public issues to fetch
ISSUES=(
    OS-6892
    TRITON-1813
    TRITON-2520
)

echo "Fetching fixtures from $BUGVIEW_URL..."

for issue in "${ISSUES[@]}"; do
    echo "  Fetching $issue..."
    cargo run -p bugview-cli --quiet -- \
        --base-url "$BUGVIEW_URL" \
        fetch-fixture "$issue" > "$FIXTURES_DIR/$issue.json"
done

echo ""
echo "Done. Fetched ${#ISSUES[@]} public issues."
echo ""
echo "Note: Non-public test fixtures (FAKE-PRIVATE-*) are hand-crafted and"
echo "should not be fetched. They are used to test that bugview correctly"
echo "filters out non-public issues."
