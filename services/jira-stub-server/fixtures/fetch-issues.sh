#!/bin/bash
# Fetch raw JIRA issue data for jira-stub-server fixtures
# Run this script with JIRA_URL, JIRA_USERNAME, and JIRA_PASSWORD set

set -e

if [[ -z "$JIRA_URL" || -z "$JIRA_USERNAME" || -z "$JIRA_PASSWORD" ]]; then
    echo "Error: JIRA_URL, JIRA_USERNAME, and JIRA_PASSWORD must be set"
    exit 1
fi

# Enforce HTTPS to protect credentials
if [[ ${JIRA_URL%%:*} != "https" ]]; then
    echo "Error: JIRA_URL must use HTTPS" >&2
    exit 1
fi

FIXTURES_DIR="$(dirname "$0")"

ISSUES=(
    OS-8627
    OS-8638
    TRITON-2497
    OS-8264
    TRITON-2378
    OS-8525
    TRITON-2520
    OS-5781
    TRITON-2524
    OS-8701
    TRITON-1813
    OS-8697
    OS-8695
    OS-8692
    MANTA-5480
    TRITON-2504
    OS-8683
    TRITON-2502
    OS-8423
    OS-8684
    TRITON-2500
    TRITON-2499
    TRITON-2494
    OS-7602
    OS-7842
    OS-8679
    OS-8674
    OS-8653
    OS-8680
    OS-8650
    OS-8678
    TOOLS-2574
    TOOLS-1218
    OS-6892
    OS-8675
    OS-8669
    OS-8645
    OS-7859
    OS-6970
    OS-8097
    OS-8667
    OS-8666
    OS-8665
    TRITON-2487
    OS-7816
    OS-8367
    OS-8655
    MANTA-3864
    OS-8334
    TRITON-2482
)

for issue in "${ISSUES[@]}"; do
    echo "Fetching $issue..."
    curl --proto =https --tlsv1.2 --fail -s -u "$JIRA_USERNAME:$JIRA_PASSWORD" \
        "$JIRA_URL/rest/api/3/issue/$issue?expand=renderedFields" \
        | jq . > "$FIXTURES_DIR/$issue.json"
done

echo "Done. Fetched ${#ISSUES[@]} issues."
