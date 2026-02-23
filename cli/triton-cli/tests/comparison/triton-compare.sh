#!/usr/bin/env bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#
# triton-compare.sh - Compare Node.js triton vs Rust triton CLI output
#
# Runs the same commands against both CLIs, normalizes output, and reports diffs.
# Exit codes: 0 = all pass, 1 = diffs found, 2 = usage error

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---------- defaults ----------
NODE_TRITON="${NODE_TRITON:-$(command -v triton 2>/dev/null || echo "")}"
RUST_TRITON="${RUST_TRITON:-target/debug/triton}"
TIER="offline"
PROFILE=""
OUTPUT_DIR=""
VERBOSE=0

# ---------- counters ----------
PASS_COUNT=0
DIFF_COUNT=0
SKIP_COUNT=0
NEW_COUNT=0
FIXED_COUNT=0

# ---------- tracking file data ----------
# Associative arrays: test_id -> bead_id or reason
declare -A KNOWN_DIFFS=()
declare -A IGNORED_DIFFS=()

# ---------- usage ----------
usage() {
    cat <<'EOF'
Usage: triton-compare.sh [OPTIONS]

Compare Node.js triton and Rust triton CLI output.

Options:
  --node-triton PATH   Path to Node.js triton (default: $(which triton))
  --rust-triton PATH   Path to Rust triton (default: target/debug/triton)
  --tier TIER          "offline", "api", or "all" (default: offline)
  --profile NAME       Profile name for API tests
  --output-dir DIR     Directory for diff artifacts (default: mktemp -d)
  --verbose            Show each command as it runs
  -h, --help           Show this help

Tiers:
  offline   Commands that need no API (help, profiles, env, completion)
  api       Read-only API commands (list, get) — requires working auth
  all       Both offline and api

Examples:
  # Quick offline comparison
  ./triton-compare.sh

  # Full comparison with API access
  ./triton-compare.sh --tier all --profile demo

  # Use specific binaries (e.g. release build)
  ./triton-compare.sh --node-triton /usr/local/bin/triton \
                      --rust-triton ./target/release/triton
EOF
}

# ---------- arg parsing ----------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --node-triton) NODE_TRITON="$2"; shift 2 ;;
        --rust-triton) RUST_TRITON="$2"; shift 2 ;;
        --tier)        TIER="$2"; shift 2 ;;
        --profile)     PROFILE="$2"; shift 2 ;;
        --output-dir)  OUTPUT_DIR="$2"; shift 2 ;;
        --verbose)     VERBOSE=1; shift ;;
        -h|--help)     usage; exit 0 ;;
        *)             echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

# ---------- validation ----------
if [[ ! "$TIER" =~ ^(offline|api|all)$ ]]; then
    echo "Error: --tier must be 'offline', 'api', or 'all'" >&2
    exit 2
fi

if [[ -z "$NODE_TRITON" ]]; then
    echo "Error: Node.js triton not found in PATH. Use --node-triton PATH." >&2
    exit 2
fi

if [[ ! -x "$NODE_TRITON" ]]; then
    echo "Error: Node.js triton not executable: $NODE_TRITON" >&2
    exit 2
fi

if [[ ! -x "$RUST_TRITON" ]]; then
    echo "Error: Rust triton not executable: $RUST_TRITON" >&2
    echo "  Build with: make build" >&2
    exit 2
fi

if [[ "$TIER" =~ ^(api|all)$ && -z "$PROFILE" ]]; then
    echo "Error: --profile required for API tests" >&2
    exit 2
fi

# ---------- load tracking files ----------

load_tracking_files() {
    local known_file="$SCRIPT_DIR/known-diffs.txt"
    local ignored_file="$SCRIPT_DIR/ignored-diffs.txt"

    if [[ -f "$known_file" ]]; then
        while IFS= read -r line; do
            # Skip comments and blank lines
            [[ "$line" =~ ^[[:space:]]*# ]] && continue
            [[ -z "${line// /}" ]] && continue
            local test_id bead_id
            test_id="$(echo "$line" | awk '{print $1}')"
            bead_id="$(echo "$line" | awk '{print $2}')"
            KNOWN_DIFFS["$test_id"]="$bead_id"
        done < "$known_file"
    fi

    if [[ -f "$ignored_file" ]]; then
        while IFS= read -r line; do
            [[ "$line" =~ ^[[:space:]]*# ]] && continue
            [[ -z "${line// /}" ]] && continue
            local test_id reason
            test_id="$(echo "$line" | awk '{print $1}')"
            reason="$(echo "$line" | awk '{$1=""; print}' | sed 's/^ //')"
            IGNORED_DIFFS["$test_id"]="$reason"
        done < "$ignored_file"
    fi
}

load_tracking_files

# ---------- setup ----------
if [[ -z "$OUTPUT_DIR" ]]; then
    OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/triton-compare.XXXXXX")"
fi
mkdir -p "$OUTPUT_DIR/diffs" "$OUTPUT_DIR/node" "$OUTPUT_DIR/rust"

# Create isolated environment for offline tests
ISOLATED_HOME="$(mktemp -d "${TMPDIR:-/tmp}/triton-compare-home.XXXXXX")"
ISOLATED_CONFIG="$ISOLATED_HOME/.triton"
mkdir -p "$ISOLATED_CONFIG/profiles.d"

# Generate SSH keys in various formats to exercise key discovery
# Each profile gets a key whose fingerprint matches its keyId
ISOLATED_SSH="$ISOLATED_HOME/.ssh"
mkdir -p "$ISOLATED_SSH"
chmod 700 "$ISOLATED_SSH"

# Key 1: ed25519 in OpenSSH format (the easy case)
ssh-keygen -t ed25519 -f "$ISOLATED_SSH/id_ed25519" -N "" -q
KEY1_FP=$(ssh-keygen -lf "$ISOLATED_SSH/id_ed25519" -E md5 | awk '{print $2}' | sed 's/^MD5://')

# Key 2: RSA in PKCS#1 PEM format (the problematic format from differences.md #1)
ssh-keygen -t rsa -b 2048 -f "$ISOLATED_SSH/id_rsa" -N "" -m PEM -q
KEY2_FP=$(ssh-keygen -lf "$ISOLATED_SSH/id_rsa" -E md5 | awk '{print $2}' | sed 's/^MD5://')

# Key 3: ECDSA in OpenSSH format
ssh-keygen -t ecdsa -b 256 -f "$ISOLATED_SSH/id_ecdsa" -N "" -q
KEY3_FP=$(ssh-keygen -lf "$ISOLATED_SSH/id_ecdsa" -E md5 | awk '{print $2}' | sed 's/^MD5://')

# Write profiles with keyIds matching the generated keys
cat > "$ISOLATED_CONFIG/profiles.d/test-compare.json" <<EOF
{
    "url": "https://cloudapi.us-test-1.example.com",
    "account": "testuser@example.com",
    "keyId": "$KEY1_FP"
}
EOF

cat > "$ISOLATED_CONFIG/profiles.d/staging.json" <<EOF
{
    "url": "https://cloudapi.staging.example.com",
    "account": "deploy@example.com",
    "keyId": "$KEY2_FP"
}
EOF

cat > "$ISOLATED_CONFIG/profiles.d/us-west-1.json" <<EOF
{
    "url": "https://cloudapi.us-west-1.example.com",
    "account": "admin@example.com",
    "keyId": "$KEY3_FP"
}
EOF

# Set test-compare as the current profile
echo '"test-compare"' > "$ISOLATED_CONFIG/profile"

cleanup() {
    rm -rf "$ISOLATED_HOME"
}
trap cleanup EXIT

NODE_VERSION=$("$NODE_TRITON" --version 2>/dev/null || echo "unknown")
RUST_VERSION=$("$RUST_TRITON" --version 2>/dev/null || echo "unknown")

echo "=== Triton CLI Comparison Report ==="
echo "Node: $NODE_TRITON ($NODE_VERSION)"
echo "Rust: $RUST_TRITON ($RUST_VERSION)"
echo "Tier: $TIER"
echo ""

# ---------- normalization functions ----------

# Strip version numbers (e.g., "7.18.0" -> "X.Y.Z", "0.1.0" -> "X.Y.Z")
normalize_version() {
    sed -E 's/[0-9]+\.[0-9]+\.[0-9]+/X.Y.Z/g'
}

# Normalize the "triton Triton CLI" clap prefix to just "Triton CLI"
normalize_cli_name() {
    sed -E 's/^triton Triton CLI/Triton CLI/'
}

# Sort JSON keys, handle NDJSON (one JSON object per line)
normalize_json() {
    local input="$1"
    # Detect NDJSON: multiple lines each starting with {
    if grep -cq '^{' "$input" 2>/dev/null && [[ $(wc -l < "$input") -gt 1 ]]; then
        # Convert NDJSON to sorted JSON array
        jq -s 'sort_by(.name // .id // .key // keys[0])' "$input" 2>/dev/null | jq -S '.' || cat "$input"
    else
        jq -S '.' "$input" 2>/dev/null || cat "$input"
    fi
}

# Collapse whitespace in table output, strip trailing spaces
normalize_table() {
    sed -E 's/[[:space:]]+/ /g; s/ $//'
}

# Strip ANSI escape codes
strip_ansi() {
    sed -E 's/\x1b\[[0-9;]*[a-zA-Z]//g'
}

# General normalization pipeline for non-JSON text output
normalize_text() {
    strip_ansi | normalize_version | normalize_cli_name
}

# Extract sorted subcommand names from help output (either Node.js or Rust format)
# Produces one "command-name" per line, sorted. Filters out "help".
# Also extracts aliases:
#   Node.js format: "copy (cp)" → outputs both "copy" and "cp"
#   Rust/clap format: "copy  Copy image [aliases: cp]" → outputs both "copy" and "cp"
# Uses POSIX awk only (no gawk, alas).
normalize_help_commands() {
    awk '
    /^Commands:/ { in_cmds=1; next }
    /^Options:/ || /^Usage:/ || /^[^ ]/ { in_cmds=0 }
    in_cmds && /^[[:space:]]+[a-z]/ {
        if ($1 == "help") next
        print $1
        # Node.js aliases: "copy (cp)" — $2 is "(alias)"
        if ($2 ~ /^\([a-z?]+\)$/) {
            alias = $2
            gsub(/[()]/, "", alias)
            if (alias != "?") print alias
        }
        # Rust/clap aliases: "[aliases: cp, ...]"
        if (match($0, /\[aliases:/)) {
            rest = substr($0, RSTART + 10)
            gsub(/\].*/, "", rest)
            n = split(rest, aliases, /,[[:space:]]*/)
            for (i = 1; i <= n; i++) {
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", aliases[i])
                if (aliases[i] != "") print aliases[i]
            }
        }
    }' | sort
}

# ---------- tracking helpers ----------

# Check if a test is in the ignored list; if so, skip it
# Returns 0 if ignored (caller should skip), 1 if not ignored
is_ignored() {
    local test_id="$1"
    [[ -v "IGNORED_DIFFS[$test_id]" ]]
}

# Get annotation for a DIFF result
diff_annotation() {
    local test_id="$1"
    if [[ -v "KNOWN_DIFFS[$test_id]" ]]; then
        echo " (bead ${KNOWN_DIFFS[$test_id]})"
    else
        echo " (NEW)"
    fi
}

# Get annotation for a PASS result that was previously a known diff
pass_annotation() {
    local test_id="$1"
    if [[ -v "KNOWN_DIFFS[$test_id]" ]]; then
        echo " (fixed? bead ${KNOWN_DIFFS[$test_id]})"
    fi
}

# ---------- test runner ----------

# Run a single command against both CLIs and compare output
# Args: test_id description [env_mode] command...
#   env_mode: "isolated" (use fake home) or "live" (use real env)
run_test() {
    local test_id="$1"; shift
    local description="$1"; shift
    local env_mode="$1"; shift
    # Remaining args are the triton subcommand + flags

    # Check ignored list first
    if is_ignored "$test_id"; then
        printf "SKIP     %-30s %s (intentional: %s)\n" \
            "$test_id" "$description" "${IGNORED_DIFFS[$test_id]}"
        SKIP_COUNT=$((SKIP_COUNT + 1))
        return 0
    fi

    if [[ $VERBOSE -eq 1 ]]; then
        echo "  Running: triton $*"
    fi

    local node_out="$OUTPUT_DIR/node/$test_id.out"
    local node_err="$OUTPUT_DIR/node/$test_id.err"
    local rust_out="$OUTPUT_DIR/rust/$test_id.out"
    local rust_err="$OUTPUT_DIR/rust/$test_id.err"

    local node_exit=0
    local rust_exit=0

    # Run Node.js triton
    if [[ "$env_mode" == "isolated" ]]; then
        HOME="$ISOLATED_HOME" TRITON_CONFIG_DIR="$ISOLATED_CONFIG" \
            "$NODE_TRITON" "$@" > "$node_out" 2> "$node_err" || node_exit=$?
    else
        "$NODE_TRITON" "$@" > "$node_out" 2> "$node_err" || node_exit=$?
    fi

    # Run Rust triton
    if [[ "$env_mode" == "isolated" ]]; then
        HOME="$ISOLATED_HOME" TRITON_CONFIG_DIR="$ISOLATED_CONFIG" \
            "$RUST_TRITON" "$@" > "$rust_out" 2> "$rust_err" || rust_exit=$?
    else
        "$RUST_TRITON" "$@" > "$rust_out" 2> "$rust_err" || rust_exit=$?
    fi

    # Normalize and diff
    compare_outputs "$test_id" "$description" "$node_out" "$rust_out" "$node_exit" "$rust_exit"
}

# Run a test with JSON output (-j flag) — uses JSON normalization
run_json_test() {
    local test_id="$1"; shift
    local description="$1"; shift
    local env_mode="$1"; shift

    # Check ignored list first
    if is_ignored "$test_id"; then
        printf "SKIP     %-30s %s (intentional: %s)\n" \
            "$test_id" "$description" "${IGNORED_DIFFS[$test_id]}"
        SKIP_COUNT=$((SKIP_COUNT + 1))
        return 0
    fi

    if [[ $VERBOSE -eq 1 ]]; then
        echo "  Running: triton $*"
    fi

    local node_out="$OUTPUT_DIR/node/$test_id.out"
    local rust_out="$OUTPUT_DIR/rust/$test_id.out"
    local node_err="$OUTPUT_DIR/node/$test_id.err"
    local rust_err="$OUTPUT_DIR/rust/$test_id.err"
    local node_norm="$OUTPUT_DIR/node/$test_id.norm"
    local rust_norm="$OUTPUT_DIR/rust/$test_id.norm"

    local node_exit=0
    local rust_exit=0

    if [[ "$env_mode" == "isolated" ]]; then
        HOME="$ISOLATED_HOME" TRITON_CONFIG_DIR="$ISOLATED_CONFIG" \
            "$NODE_TRITON" "$@" > "$node_out" 2> "$node_err" || node_exit=$?
        HOME="$ISOLATED_HOME" TRITON_CONFIG_DIR="$ISOLATED_CONFIG" \
            "$RUST_TRITON" "$@" > "$rust_out" 2> "$rust_err" || rust_exit=$?
    else
        "$NODE_TRITON" "$@" > "$node_out" 2> "$node_err" || node_exit=$?
        "$RUST_TRITON" "$@" > "$rust_out" 2> "$rust_err" || rust_exit=$?
    fi

    # Normalize JSON
    normalize_json "$node_out" > "$node_norm"
    normalize_json "$rust_out" > "$rust_norm"

    compare_files "$test_id" "$description" "$node_norm" "$rust_norm" "$node_exit" "$rust_exit"
}

# Compare two already-produced output files with text normalization
compare_outputs() {
    local test_id="$1"
    local description="$2"
    local node_out="$3"
    local rust_out="$4"
    local node_exit="$5"
    local rust_exit="$6"

    local node_norm="$OUTPUT_DIR/node/$test_id.norm"
    local rust_norm="$OUTPUT_DIR/rust/$test_id.norm"

    normalize_text < "$node_out" > "$node_norm"
    normalize_text < "$rust_out" > "$rust_norm"

    compare_files "$test_id" "$description" "$node_norm" "$rust_norm" "$node_exit" "$rust_exit"
}

# Core comparison of two normalized files
compare_files() {
    local test_id="$1"
    local description="$2"
    local node_norm="$3"
    local rust_norm="$4"
    local node_exit="$5"
    local rust_exit="$6"

    local diff_file="$OUTPUT_DIR/diffs/$test_id.diff"

    # Build diff with context
    local has_diff=0
    {
        if [[ "$node_exit" -ne "$rust_exit" ]]; then
            echo "Exit code: node=$node_exit rust=$rust_exit"
            echo "---"
            has_diff=1
        fi
        if ! diff -u --label "node" --label "rust" "$node_norm" "$rust_norm"; then
            has_diff=1
        fi
    } > "$diff_file" 2>&1

    if [[ $has_diff -eq 0 ]]; then
        local annotation
        annotation="$(pass_annotation "$test_id")"
        printf "PASS     %-30s %s%s\n" "$test_id" "$description" "$annotation"
        PASS_COUNT=$((PASS_COUNT + 1))
        if [[ -n "$annotation" ]]; then
            FIXED_COUNT=$((FIXED_COUNT + 1))
        fi
        rm -f "$diff_file"
    else
        local annotation
        annotation="$(diff_annotation "$test_id")"
        printf "DIFF     %-30s %s%s\n" "$test_id" "$description" "$annotation"
        DIFF_COUNT=$((DIFF_COUNT + 1))
        if [[ "$annotation" == " (NEW)" ]]; then
            NEW_COUNT=$((NEW_COUNT + 1))
        fi
    fi
}

# Skip a test with a reason
skip_test() {
    local test_id="$1"
    local description="$2"
    local reason="$3"
    printf "SKIP     %-30s %s (%s)\n" "$test_id" "$description" "$reason"
    SKIP_COUNT=$((SKIP_COUNT + 1))
}

# Run a help coverage test: compare subcommand names, not exact help text
# This verifies the Rust CLI has the same subcommands as Node.js, ignoring
# layout differences (clap vs node-cmdln).
run_help_coverage_test() {
    local test_id="$1"; shift
    local description="$1"; shift
    local env_mode="$1"; shift
    # Remaining args are the triton subcommand + flags

    # Check ignored list first
    if is_ignored "$test_id"; then
        printf "SKIP     %-30s %s (intentional: %s)\n" \
            "$test_id" "$description" "${IGNORED_DIFFS[$test_id]}"
        SKIP_COUNT=$((SKIP_COUNT + 1))
        return 0
    fi

    if [[ $VERBOSE -eq 1 ]]; then
        echo "  Running: triton $*"
    fi

    local node_out="$OUTPUT_DIR/node/$test_id.out"
    local node_err="$OUTPUT_DIR/node/$test_id.err"
    local rust_out="$OUTPUT_DIR/rust/$test_id.out"
    local rust_err="$OUTPUT_DIR/rust/$test_id.err"

    local node_exit=0
    local rust_exit=0

    # Run both CLIs
    if [[ "$env_mode" == "isolated" ]]; then
        HOME="$ISOLATED_HOME" TRITON_CONFIG_DIR="$ISOLATED_CONFIG" \
            "$NODE_TRITON" "$@" > "$node_out" 2> "$node_err" || node_exit=$?
        HOME="$ISOLATED_HOME" TRITON_CONFIG_DIR="$ISOLATED_CONFIG" \
            "$RUST_TRITON" "$@" > "$rust_out" 2> "$rust_err" || rust_exit=$?
    else
        "$NODE_TRITON" "$@" > "$node_out" 2> "$node_err" || node_exit=$?
        "$RUST_TRITON" "$@" > "$rust_out" 2> "$rust_err" || rust_exit=$?
    fi

    # Extract subcommand names
    local node_norm="$OUTPUT_DIR/node/$test_id.norm"
    local rust_norm="$OUTPUT_DIR/rust/$test_id.norm"

    normalize_help_commands < "$node_out" > "$node_norm"
    normalize_help_commands < "$rust_out" > "$rust_norm"

    # Superset check: every Node.js command must exist in Rust.
    # Extra Rust commands are OK (improvements). Only missing ones are flagged.
    local missing
    missing="$(comm -23 "$node_norm" "$rust_norm")"

    local diff_file="$OUTPUT_DIR/diffs/$test_id.diff"

    if [[ -z "$missing" ]]; then
        local annotation
        annotation="$(pass_annotation "$test_id")"
        printf "PASS     %-30s %s%s\n" "$test_id" "$description" "$annotation"
        PASS_COUNT=$((PASS_COUNT + 1))
        if [[ -n "$annotation" ]]; then
            FIXED_COUNT=$((FIXED_COUNT + 1))
        fi
        rm -f "$diff_file"
    else
        {
            echo "Commands in Node.js but missing from Rust:"
            echo "$missing" | sed 's/^/  /'
            local extra
            extra="$(comm -13 "$node_norm" "$rust_norm")"
            if [[ -n "$extra" ]]; then
                echo ""
                echo "Extra commands in Rust (OK):"
                echo "$extra" | sed 's/^/  /'
            fi
        } > "$diff_file"
        local annotation
        annotation="$(diff_annotation "$test_id")"
        printf "DIFF     %-30s %s%s\n" "$test_id" "$description" "$annotation"
        DIFF_COUNT=$((DIFF_COUNT + 1))
        if [[ "$annotation" == " (NEW)" ]]; then
            NEW_COUNT=$((NEW_COUNT + 1))
        fi
    fi
}

# ---------- Tier 1: Offline tests ----------

run_offline_tests() {
    echo "--- Offline Tests ---"
    echo ""
    printf "%-8s %-30s %s\n" "RESULT" "TEST ID" "DESCRIPTION"
    printf "%-8s %-30s %s\n" "------" "-------" "-----------"

    # Version
    run_test "version" "--version output" \
        isolated --version

    # Top-level help
    run_test "help" "top-level --help" \
        isolated --help

    # Profile commands (isolated environment)
    run_test "profile-list" "profile list" \
        isolated profile list

    run_json_test "profile-list-json" "profile list -j" \
        isolated profile list -j

    run_test "profile-get" "profile get" \
        isolated profile get

    run_json_test "profile-get-json" "profile get -j" \
        isolated profile get -j

    # Env command
    run_test "env-bash" "env (bash output)" \
        isolated env

    # Check if fish shell is supported by both
    run_test "env-fish" "env --shell fish" \
        isolated env --shell fish 2>/dev/null || \
    run_test "env-fish" "env --shell fish" \
        isolated env -s fish 2>/dev/null || \
    skip_test "env-fish" "env --shell fish" "flag syntax differs"

    # Completion
    run_test "completion-bash" "completion bash" \
        isolated completion bash 2>/dev/null || \
    skip_test "completion-bash" "completion bash" "syntax differs"

    # Subcommand help — compare command coverage, not layout
    for subcmd in instance image package network volume key fwrule vlan account; do
        run_help_coverage_test "help-$subcmd" "$subcmd --help" \
            isolated "$subcmd" --help
    done

    echo ""
}

# ---------- Tier 2: API tests ----------

run_api_tests() {
    echo "--- API Tests (read-only, profile: $PROFILE) ---"
    echo ""
    printf "%-8s %-30s %s\n" "RESULT" "TEST ID" "DESCRIPTION"
    printf "%-8s %-30s %s\n" "------" "-------" "-----------"

    # List commands (table + JSON)
    for resource in instance image package network volume key fwrule vlan; do
        run_test "${resource}-list" "$resource list" \
            live -p "$PROFILE" "$resource" list

        run_json_test "${resource}-list-json" "$resource list -j" \
            live -p "$PROFILE" "$resource" list -j
    done

    # Account and info
    run_test "account-get" "account get" \
        live -p "$PROFILE" account get

    run_json_test "account-get-json" "account get -j" \
        live -p "$PROFILE" account get -j

    run_test "info" "info" \
        live -p "$PROFILE" info

    run_json_test "info-json" "info -j" \
        live -p "$PROFILE" info -j

    # Datacenters and services
    run_test "datacenters" "datacenters" \
        live -p "$PROFILE" datacenters

    run_json_test "datacenters-json" "datacenters -j" \
        live -p "$PROFILE" datacenters -j

    run_test "services" "services" \
        live -p "$PROFILE" services

    run_json_test "services-json" "services -j" \
        live -p "$PROFILE" services -j

    # Dynamic get tests: parse first ID from JSON list, then run get
    for resource in instance image package network volume key; do
        local first_id
        first_id=$("$NODE_TRITON" -p "$PROFILE" "$resource" list -j 2>/dev/null \
            | head -1 | jq -r '.id // .name // .key // empty' 2>/dev/null || echo "")

        if [[ -n "$first_id" ]]; then
            # Both CLIs output JSON for resource get (even without -j),
            # so use JSON normalization to sort keys consistently
            run_json_test "${resource}-get" "$resource get $first_id" \
                live -p "$PROFILE" "$resource" get "$first_id"

            run_json_test "${resource}-get-json" "$resource get -j $first_id" \
                live -p "$PROFILE" "$resource" get -j "$first_id"
        else
            skip_test "${resource}-get" "$resource get" "no items found"
            skip_test "${resource}-get-json" "$resource get -j" "no items found"
        fi
    done

    # Alias tests: verify shorthand commands produce same output as long form
    echo ""
    echo "--- Alias Tests ---"
    echo ""
    printf "%-8s %-30s %s\n" "RESULT" "TEST ID" "DESCRIPTION"
    printf "%-8s %-30s %s\n" "------" "-------" "-----------"

    # For aliases, compare Rust short vs Rust long (not node vs rust)
    for pair in "insts:instance list" "imgs:image list" "pkgs:package list" "nets:network list"; do
        local alias_cmd="${pair%%:*}"
        local long_cmd="${pair#*:}"
        local test_id="alias-$alias_cmd"

        local alias_out="$OUTPUT_DIR/rust/${test_id}-alias.out"
        local long_out="$OUTPUT_DIR/rust/${test_id}-long.out"

        "$RUST_TRITON" -p "$PROFILE" $alias_cmd -j > "$alias_out" 2>/dev/null || true
        # shellcheck disable=SC2086
        "$RUST_TRITON" -p "$PROFILE" $long_cmd -j > "$long_out" 2>/dev/null || true

        local alias_norm="$OUTPUT_DIR/rust/${test_id}-alias.norm"
        local long_norm="$OUTPUT_DIR/rust/${test_id}-long.norm"

        normalize_json "$alias_out" > "$alias_norm"
        normalize_json "$long_out" > "$long_norm"

        if diff -q "$alias_norm" "$long_norm" > /dev/null 2>&1; then
            printf "PASS     %-30s %s\n" "$test_id" "$alias_cmd == $long_cmd"
            PASS_COUNT=$((PASS_COUNT + 1))
        else
            printf "DIFF     %-30s %s\n" "$test_id" "$alias_cmd != $long_cmd"
            diff -u --label "alias" --label "long" "$alias_norm" "$long_norm" \
                > "$OUTPUT_DIR/diffs/${test_id}.diff" 2>&1 || true
            DIFF_COUNT=$((DIFF_COUNT + 1))
        fi
    done

    echo ""
}

# ---------- main ----------

case "$TIER" in
    offline) run_offline_tests ;;
    api)     run_api_tests ;;
    all)     run_offline_tests; run_api_tests ;;
esac

echo "=== Summary ==="
echo "  Pass: $PASS_COUNT"
echo "  Diff: $DIFF_COUNT (known: $((DIFF_COUNT - NEW_COUNT)), new: $NEW_COUNT)"
echo "  Skip: $SKIP_COUNT"
if [[ $FIXED_COUNT -gt 0 ]]; then
    echo "  Fixed: $FIXED_COUNT (known diffs now passing — close the bead!)"
fi
echo ""

if [[ $DIFF_COUNT -gt 0 ]]; then
    echo "Diffs saved to: $OUTPUT_DIR/diffs/"
    echo ""
    echo "To inspect a diff:"
    echo "  cat $OUTPUT_DIR/diffs/<test-id>.diff"
    echo ""
    echo "Raw outputs saved to:"
    echo "  $OUTPUT_DIR/node/  (Node.js)"
    echo "  $OUTPUT_DIR/rust/  (Rust)"
    if [[ $NEW_COUNT -gt 0 ]]; then
        echo ""
        echo "New diffs need triage — file a bead or add to ignored-diffs.txt"
    fi
    exit 1
else
    echo "All tests passed!"
    # Clean up output dir if no diffs (nothing to inspect)
    if [[ "$OUTPUT_DIR" == *triton-compare.* ]]; then
        rm -rf "$OUTPUT_DIR"
    fi
    exit 0
fi
