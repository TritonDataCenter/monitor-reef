#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)

usage() {
    cat <<'EOF'
usage: scripts/tritond-fdb.sh [tritond-args...]

Run a FoundationDB-enabled tritond binary with libfdb_c.so discoverable
on SmartOS/illumos lab hosts.

Environment:
  TRITOND_BIN         tritond binary to exec
                      default: target/debug/tritond, then /opt/tritond/bin/tritond
  FDB_CLIENT_LIB_DIR  directory containing libfdb_c.so
                      default: auto-detect common lab/pkgsrc paths

Example:
  scripts/tritond-fdb.sh reset-root-password --fdb-cluster-file /etc/fdb.cluster
EOF
}

die() {
    printf 'tritond-fdb: %s\n' "$*" >&2
    exit 1
}

detect_tritond() {
    if [[ -n "${TRITOND_BIN:-}" ]]; then
        [[ -x "${TRITOND_BIN}" ]] || die "TRITOND_BIN is not executable: ${TRITOND_BIN}"
        printf '%s\n' "${TRITOND_BIN}"
    elif [[ -x "${REPO_ROOT}/target/debug/tritond" ]]; then
        printf '%s\n' "${REPO_ROOT}/target/debug/tritond"
    elif [[ -x "/opt/tritond/bin/tritond" ]]; then
        printf '%s\n' "/opt/tritond/bin/tritond"
    elif command -v tritond >/dev/null 2>&1; then
        command -v tritond
    else
        die "could not find tritond; set TRITOND_BIN=/path/to/tritond"
    fi
}

detect_fdb_lib_dir() {
    local candidates=(
        "${FDB_CLIENT_LIB_DIR:-}"
        /opt/fdb/lib
        /opt/foundationdb/lib
        /opt/local/lib
        /usr/local/lib
        /usr/lib/amd64
        /lib/amd64
    )
    local dir
    for dir in "${candidates[@]}"; do
        [[ -n "${dir}" ]] || continue
        if [[ -r "${dir}/libfdb_c.so" ]]; then
            printf '%s\n' "${dir}"
            return 0
        fi
    done
    return 1
}

prepend_path() {
    local dir=$1
    local current=${2:-}
    if [[ -n "${current}" ]]; then
        printf '%s:%s\n' "${dir}" "${current}"
    else
        printf '%s\n' "${dir}"
    fi
}

main() {
    if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
        usage
        exit 0
    fi

    local tritond_bin
    local fdb_lib_dir
    tritond_bin=$(detect_tritond)
    fdb_lib_dir=$(detect_fdb_lib_dir) || die \
        "could not find libfdb_c.so; install foundationdb-clients or set FDB_CLIENT_LIB_DIR=/path/to/lib"

    export LD_LIBRARY_PATH
    export LD_LIBRARY_PATH_64
    LD_LIBRARY_PATH=$(prepend_path "${fdb_lib_dir}" "${LD_LIBRARY_PATH:-}")
    LD_LIBRARY_PATH_64=$(prepend_path "${fdb_lib_dir}" "${LD_LIBRARY_PATH_64:-}")

    exec "${tritond_bin}" "$@"
}

main "$@"
